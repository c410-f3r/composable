// std
use std::sync::Arc;

// Local Runtime Types
use picasso_runtime::{self as parachain_runtime, RuntimeApi};

// Cumulus Imports
use common::{AccountId, AccountIndex, Balance};
use cumulus_client_consensus_aura::{
	build_aura_consensus, BuildAuraConsensusParams, SlotProportion,
};
use cumulus_client_consensus_common::ParachainConsensus;
use cumulus_client_network::build_block_announce_validator;
use cumulus_client_service::{
	prepare_node_config, start_collator, start_full_node, StartCollatorParams, StartFullNodeParams,
};
use cumulus_primitives_core::ParaId;

// Substrate Imports
use crate::rpc;
use sc_client_api::ExecutorProvider;
use sc_executor::NativeElseWasmExecutor;
use sc_network::NetworkService;
use sc_service::{Configuration, PartialComponents, Role, TFullBackend, TFullClient, TaskManager};
use sc_telemetry::{Telemetry, TelemetryHandle, TelemetryWorker, TelemetryWorkerHandle};
use sp_api::ConstructRuntimeApi;
use sp_consensus::SlotData;
#[cfg(feature = "ocw")]
use sp_core::crypto::KeyTypeId;
use sp_keystore::SyncCryptoStorePtr;
use sp_runtime::traits::BlakeTwo256;
use substrate_prometheus_endpoint::Registry;

// Runtime type overrides
type BlockNumber = u32;
type Header = sp_runtime::generic::Header<BlockNumber, sp_runtime::traits::BlakeTwo256>;
pub type Block = sp_runtime::generic::Block<Header, sp_runtime::OpaqueExtrinsic>;
type Hash = sp_core::H256;

pub struct Executor;

impl sc_executor::NativeExecutionDispatch for Executor {
	type ExtendHostFunctions = frame_benchmarking::benchmarking::HostFunctions;

	fn dispatch(method: &str, data: &[u8]) -> Option<Vec<u8>> {
		parachain_runtime::api::dispatch(method, data)
	}

	fn native_version() -> sc_executor::NativeVersion {
		parachain_runtime::native_version()
	}
}

/// Starts a `ServiceBuilder` for a full service.
///
/// Use this macro if you don't actually need the full service, but just the builder in order to
/// be able to perform chain operations.
#[allow(clippy::type_complexity)]
pub fn new_partial<RuntimeApi, Executor, BIQ>(
	config: &Configuration,
	build_import_queue: BIQ,
) -> Result<
	PartialComponents<
		TFullClient<Block, RuntimeApi, NativeElseWasmExecutor<Executor>>,
		TFullBackend<Block>,
		(),
		sc_consensus::DefaultImportQueue<
			Block,
			TFullClient<Block, RuntimeApi, NativeElseWasmExecutor<Executor>>,
		>,
		sc_transaction_pool::FullPool<
			Block,
			TFullClient<Block, RuntimeApi, NativeElseWasmExecutor<Executor>>,
		>,
		(Option<Telemetry>, Option<TelemetryWorkerHandle>),
	>,
	sc_service::Error,
>
where
	RuntimeApi: ConstructRuntimeApi<Block, TFullClient<Block, RuntimeApi, NativeElseWasmExecutor<Executor>>>
		+ Send
		+ Sync
		+ 'static,
	RuntimeApi::RuntimeApi: sp_transaction_pool::runtime_api::TaggedTransactionQueue<Block>
		+ sp_api::Metadata<Block>
		+ sp_session::SessionKeys<Block>
		+ sp_api::ApiExt<
			Block,
			StateBackend = sc_client_api::StateBackendFor<TFullBackend<Block>, Block>,
		> + sp_offchain::OffchainWorkerApi<Block>
		+ sp_block_builder::BlockBuilder<Block>,
	sc_client_api::StateBackendFor<TFullBackend<Block>, Block>: sp_api::StateBackend<BlakeTwo256>,
	Executor: sc_executor::NativeExecutionDispatch + 'static,
	BIQ: FnOnce(
		Arc<TFullClient<Block, RuntimeApi, NativeElseWasmExecutor<Executor>>>,
		&Configuration,
		Option<TelemetryHandle>,
		&TaskManager,
	) -> Result<
		sc_consensus::DefaultImportQueue<
			Block,
			TFullClient<Block, RuntimeApi, NativeElseWasmExecutor<Executor>>,
		>,
		sc_service::Error,
	>,
{
	let telemetry = config
		.telemetry_endpoints
		.clone()
		.filter(|x| !x.is_empty())
		.map(|endpoints| -> Result<_, sc_telemetry::Error> {
			let worker = TelemetryWorker::new(16)?;
			let telemetry = worker.handle().new_telemetry(endpoints);
			Ok((worker, telemetry))
		})
		.transpose()?;

	let executor = sc_executor::NativeElseWasmExecutor::<Executor>::new(
		config.wasm_method,
		config.default_heap_pages,
		config.max_runtime_instances,
	);

	let (client, backend, keystore_container, task_manager) =
		sc_service::new_full_parts::<Block, RuntimeApi, _>(
			config,
			telemetry.as_ref().map(|(_, telemetry)| telemetry.handle()),
			executor,
		)?;
	let client = Arc::new(client);

	let telemetry_worker_handle = telemetry.as_ref().map(|(worker, _)| worker.handle());

	let telemetry = telemetry.map(|(worker, telemetry)| {
		task_manager.spawn_handle().spawn("telemetry", worker.run());
		telemetry
	});

	let transaction_pool = sc_transaction_pool::BasicPool::new_full(
		config.transaction_pool.clone(),
		config.role.is_authority().into(),
		config.prometheus_registry(),
		task_manager.spawn_essential_handle(),
		client.clone(),
	);

	let import_queue = build_import_queue(
		client.clone(),
		config,
		telemetry.as_ref().map(|telemetry| telemetry.handle()),
		&task_manager,
	)?;

	let params = PartialComponents {
		backend,
		client,
		import_queue,
		keystore_container,
		task_manager,
		transaction_pool,
		select_chain: (),
		other: (telemetry, telemetry_worker_handle),
	};

	Ok(params)
}

/// Start a node with the given parachain `Configuration` and relay chain `Configuration`.
///
/// This is the actual implementation that is abstract over the executor and the runtime api.
#[sc_tracing::logging::prefix_logs_with("Parachain")]
async fn start_node_impl<RuntimeApi, Executor, BIQ, BIC>(
	parachain_config: Configuration,
	polkadot_config: Configuration,
	id: ParaId,
	build_import_queue: BIQ,
	build_consensus: BIC,
) -> sc_service::error::Result<(
	TaskManager,
	Arc<TFullClient<Block, RuntimeApi, NativeElseWasmExecutor<Executor>>>,
)>
where
	RuntimeApi: ConstructRuntimeApi<Block, TFullClient<Block, RuntimeApi, NativeElseWasmExecutor<Executor>>>
		+ Send
		+ Sync
		+ 'static,
	RuntimeApi::RuntimeApi: sp_transaction_pool::runtime_api::TaggedTransactionQueue<Block>
		+ sp_api::Metadata<Block>
		+ sp_session::SessionKeys<Block>
		+ sp_api::ApiExt<
			Block,
			StateBackend = sc_client_api::StateBackendFor<TFullBackend<Block>, Block>,
		> + sp_offchain::OffchainWorkerApi<Block>
		+ sp_block_builder::BlockBuilder<Block>
		+ cumulus_primitives_core::CollectCollationInfo<Block>
		+ substrate_frame_rpc_system::AccountNonceApi<Block, AccountId, AccountIndex>
		+ pallet_transaction_payment_rpc::TransactionPaymentRuntimeApi<Block, Balance>,
	sc_client_api::StateBackendFor<TFullBackend<Block>, Block>: sp_api::StateBackend<BlakeTwo256>,
	Executor: sc_executor::NativeExecutionDispatch + 'static,
	BIQ: FnOnce(
		Arc<TFullClient<Block, RuntimeApi, NativeElseWasmExecutor<Executor>>>,
		&Configuration,
		Option<TelemetryHandle>,
		&TaskManager,
	) -> Result<
		sc_consensus::DefaultImportQueue<
			Block,
			TFullClient<Block, RuntimeApi, NativeElseWasmExecutor<Executor>>,
		>,
		sc_service::Error,
	>,
	BIC: FnOnce(
		Arc<TFullClient<Block, RuntimeApi, NativeElseWasmExecutor<Executor>>>,
		Option<&Registry>,
		Option<TelemetryHandle>,
		&TaskManager,
		&polkadot_service::NewFull<polkadot_service::Client>,
		Arc<
			sc_transaction_pool::FullPool<
				Block,
				TFullClient<Block, RuntimeApi, NativeElseWasmExecutor<Executor>>,
			>,
		>,
		Arc<NetworkService<Block, Hash>>,
		SyncCryptoStorePtr,
		bool,
	) -> Result<Box<dyn ParachainConsensus<Block>>, sc_service::Error>,
{
	if matches!(parachain_config.role, Role::Light) {
		return Err("Light client not supported!".into())
	}

	let parachain_config = prepare_node_config(parachain_config);

	let params = new_partial::<RuntimeApi, Executor, BIQ>(&parachain_config, build_import_queue)?;

	#[cfg(feature = "ocw")]
	{
		let keystore = params.keystore_container.sync_keystore();
		if parachain_config.offchain_worker.enabled {
			// Initialize seed for signing transaction using off-chain workers. This is a
			// convenience so learners can see the transactions submitted simply running the node.
			// Typically these keys should be inserted with RPC calls to `author_insertKey`.
			// TODO(Jesse): remove in prod
			{
				sp_keystore::SyncCryptoStore::sr25519_generate_new(
					&*keystore,
					KeyTypeId(*b"orac"),
					Some("//Alice"),
				)
				.expect("Creating key with account Alice should succeed.");
			}
		}
	}

	let (mut telemetry, telemetry_worker_handle) = params.other;

	let relay_chain_full_node =
		cumulus_client_service::build_polkadot_full_node(polkadot_config, telemetry_worker_handle)
			.map_err(|e| match e {
				polkadot_service::Error::Sub(x) => x,
				s => format!("{}", s).into(),
			})?;

	let client = params.client.clone();
	let backend = params.backend.clone();
	let block_announce_validator = build_block_announce_validator(
		relay_chain_full_node.client.clone(),
		id,
		Box::new(relay_chain_full_node.network.clone()),
		relay_chain_full_node.backend.clone(),
	);

	let force_authoring = parachain_config.force_authoring;
	let validator = parachain_config.role.is_authority();
	let prometheus_registry = parachain_config.prometheus_registry().cloned();
	let transaction_pool = params.transaction_pool.clone();
	let mut task_manager = params.task_manager;
	let import_queue = cumulus_client_service::SharedImportQueue::new(params.import_queue);
	let (network, system_rpc_tx, start_network) =
		sc_service::build_network(sc_service::BuildNetworkParams {
			config: &parachain_config,
			client: client.clone(),
			transaction_pool: transaction_pool.clone(),
			spawn_handle: task_manager.spawn_handle(),
			import_queue: import_queue.clone(),
			on_demand: None,
			warp_sync: None,
			block_announce_validator_builder: Some(Box::new(|_| block_announce_validator)),
		})?;

	if parachain_config.offchain_worker.enabled {
		sc_service::build_offchain_workers(
			&parachain_config,
			task_manager.spawn_handle(),
			client.clone(),
			network.clone(),
		);
	}

	let rpc_extensions_builder = {
		let client = client.clone();
		let transaction_pool = transaction_pool.clone();
		Box::new(move |deny_unsafe, _| {
			let deps = rpc::FullDeps {
				client: client.clone(),
				pool: transaction_pool.clone(),
				deny_unsafe,
			};

			Ok(rpc::create(deps))
		})
	};

	sc_service::spawn_tasks(sc_service::SpawnTasksParams {
		on_demand: None,
		remote_blockchain: None,
		rpc_extensions_builder,
		client: client.clone(),
		transaction_pool: transaction_pool.clone(),
		task_manager: &mut task_manager,
		config: parachain_config,
		keystore: params.keystore_container.sync_keystore(),
		backend: backend.clone(),
		network: network.clone(),
		system_rpc_tx,
		telemetry: telemetry.as_mut(),
	})?;

	let announce_block = {
		let network = network.clone();
		Arc::new(move |hash, data| network.announce_block(hash, data))
	};

	if validator {
		let parachain_consensus = build_consensus(
			client.clone(),
			prometheus_registry.as_ref(),
			telemetry.as_ref().map(|t| t.handle()),
			&task_manager,
			&relay_chain_full_node,
			transaction_pool,
			network,
			params.keystore_container.sync_keystore(),
			force_authoring,
		)?;

		let spawner = task_manager.spawn_handle();

		let params = StartCollatorParams {
			para_id: id,
			block_status: client.clone(),
			announce_block,
			client: client.clone(),
			task_manager: &mut task_manager,
			relay_chain_full_node,
			spawner,
			parachain_consensus,
			import_queue,
		};

		start_collator(params).await?;
	} else {
		let params = StartFullNodeParams {
			client: client.clone(),
			announce_block,
			task_manager: &mut task_manager,
			para_id: id,
			relay_chain_full_node,
		};

		start_full_node(params)?;
	}

	start_network.start_network();

	Ok((task_manager, client))
}

/// Build the import queue for the the parachain runtime.
#[allow(clippy::type_complexity)]
pub fn parachain_build_import_queue(
	client: Arc<TFullClient<Block, RuntimeApi, NativeElseWasmExecutor<Executor>>>,
	config: &Configuration,
	telemetry: Option<TelemetryHandle>,
	task_manager: &TaskManager,
) -> Result<
	sc_consensus::DefaultImportQueue<
		Block,
		TFullClient<Block, RuntimeApi, NativeElseWasmExecutor<Executor>>,
	>,
	sc_service::Error,
> {
	let slot_duration = cumulus_client_consensus_aura::slot_duration(&*client)?;

	cumulus_client_consensus_aura::import_queue::<
		sp_consensus_aura::sr25519::AuthorityPair,
		_,
		_,
		_,
		_,
		_,
		_,
	>(cumulus_client_consensus_aura::ImportQueueParams {
		block_import: client.clone(),
		client: client.clone(),
		create_inherent_data_providers: move |_, _| async move {
			let time = sp_timestamp::InherentDataProvider::from_system_time();

			let slot =
				sp_consensus_aura::inherents::InherentDataProvider::from_timestamp_and_duration(
					*time,
					slot_duration.slot_duration(),
				);

			Ok((time, slot))
		},
		registry: config.prometheus_registry(),
		can_author_with: sp_consensus::CanAuthorWithNativeVersion::new(client.executor().clone()),
		spawner: &task_manager.spawn_essential_handle(),
		telemetry,
	})
	.map_err(Into::into)
}

/// Start a normal parachain node.
pub async fn start_node(
	parachain_config: Configuration,
	polkadot_config: Configuration,
	id: ParaId,
) -> sc_service::error::Result<(
	TaskManager,
	Arc<TFullClient<Block, RuntimeApi, NativeElseWasmExecutor<Executor>>>,
)> {
	start_node_impl::<RuntimeApi, Executor, _, _>(
		parachain_config,
		polkadot_config,
		id,
		parachain_build_import_queue,
		|client,
		 prometheus_registry,
		 telemetry,
		 task_manager,
		 relay_chain_node,
		 transaction_pool,
		 sync_oracle,
		 keystore,
		 force_authoring| {
			let slot_duration = cumulus_client_consensus_aura::slot_duration(&*client)?;

			let proposer_factory = sc_basic_authorship::ProposerFactory::with_proof_recording(
				task_manager.spawn_handle(),
				client.clone(),
				transaction_pool,
				prometheus_registry,
				telemetry.clone(),
			);

			let relay_chain_backend = relay_chain_node.backend.clone();
			let relay_chain_client = relay_chain_node.client.clone();
			Ok(build_aura_consensus::<
				sp_consensus_aura::sr25519::AuthorityPair,
				_,
				_,
				_,
				_,
				_,
				_,
				_,
				_,
				_,
			>(BuildAuraConsensusParams {
				proposer_factory,
				create_inherent_data_providers: move |_, (relay_parent, validation_data)| {
					let parachain_inherent =
					cumulus_primitives_parachain_inherent::ParachainInherentData::create_at_with_client(
						relay_parent,
						&relay_chain_client,
						&*relay_chain_backend,
						&validation_data,
						id,
					);
					async move {
						let time = sp_timestamp::InherentDataProvider::from_system_time();

						let slot =
						sp_consensus_aura::inherents::InherentDataProvider::from_timestamp_and_duration(
							*time,
							slot_duration.slot_duration(),
						);

						let parachain_inherent = parachain_inherent.ok_or_else(|| {
							Box::<dyn std::error::Error + Send + Sync>::from(
								"Failed to create parachain inherent",
							)
						})?;
						Ok((time, slot, parachain_inherent))
					}
				},
				block_import: client.clone(),
				relay_chain_client: relay_chain_node.client.clone(),
				relay_chain_backend: relay_chain_node.backend.clone(),
				para_client: client,
				backoff_authoring_blocks: Option::<()>::None,
				sync_oracle,
				keystore,
				force_authoring,
				slot_duration,
				// We got around 500ms for proposing
				block_proposal_slot_portion: SlotProportion::new(1f32 / 24f32),
				// And a maximum of 750ms if slots are skipped
				max_block_proposal_slot_portion: Some(SlotProportion::new(1f32 / 16f32)),
				telemetry,
			}))
		},
	)
	.await
}
