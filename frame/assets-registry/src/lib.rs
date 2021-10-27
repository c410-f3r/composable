#![cfg_attr(not(feature = "std"), no_std)]

/// Edit this file to define custom logic or remove it if it is not needed.
/// Learn more about FRAME and the core library of Substrate FRAME pallets:
/// <https://substrate.dev/docs/en/knowledgebase/runtime/frame>
pub use pallet::*;

#[cfg(test)]
mod mock;

#[cfg(test)]
mod tests;

#[frame_support::pallet]
pub mod pallet {
	use codec::FullCodec;
	use frame_support::{
		dispatch::DispatchResultWithPostInfo,
		pallet_prelude::*,
		traits::EnsureOrigin,
	};

	use frame_system::pallet_prelude::*;
	use scale_info::TypeInfo;
	use sp_std::{fmt::Debug, marker::PhantomData, str};

	#[pallet::config]
	pub trait Config: frame_system::Config {
		type Event: From<Event<Self>> + IsType<<Self as frame_system::Config>::Event>;
		type LocalAssetId: FullCodec
			+ Eq
			+ PartialEq
			+ Copy
			+ MaybeSerializeDeserialize
			+ From<u128>
			+ Into<u128>
			+ Debug
			+ Default
			+ TypeInfo;
		type ForeignAssetId: FullCodec
			+ Eq
			+ PartialEq
			+ Copy
			+ MaybeSerializeDeserialize
			+ From<u128>
			+ Into<u128>
			+ Debug
			+ Default
			+ TypeInfo;
		type UpdateAdmin: EnsureOrigin<Self::Origin>;
		type LocalAdminOrigin: EnsureOrigin<Self::Origin>;
		type ForeignAdminOrigin: EnsureOrigin<Self::Origin>;
	}

	#[derive(Debug, Clone, Copy, Encode, Decode, TypeInfo)]
	pub enum CandidateStatus {
		NobodyApproved,
		LocalAdminApproved,
		ForeignAdminApproved,
	}

	#[pallet::pallet]
	#[pallet::generate_store(pub(super) trait Store)]
	pub struct Pallet<T>(_);

	#[pallet::storage]
	#[pallet::getter(fn local_admin)]
	/// Local admin account
	pub type LocalAdmin<T: Config> = StorageValue<_, T::AccountId, ValueQuery>;

	#[pallet::storage]
	#[pallet::getter(fn foreign_admin)]
	/// Foreign admin account
	pub type ForeignAdmin<T: Config> = StorageValue<_, T::AccountId, ValueQuery>;

	#[pallet::storage]
	#[pallet::getter(fn from_local_asset)]
	/// Mapping local asset to foreign asset.
	pub type LocalAsset<T: Config> =
		StorageMap<_, Blake2_128Concat, T::LocalAssetId, T::ForeignAssetId, OptionQuery>;

	#[pallet::storage]
	#[pallet::getter(fn from_foreign_asset)]
	/// Mapping foreign asset to local asset.
	pub type ForeignAsset<T: Config> =
		StorageMap<_, Blake2_128Concat, T::ForeignAssetId, T::LocalAssetId, OptionQuery>;

	#[pallet::storage]
	#[pallet::getter(fn assets_mapping_candidates)]
	/// Mapping (local asset, foreign asset) to candidate status.
	pub type AssetsMappingCandidates<T: Config> =
		StorageMap<_, Blake2_128Concat, (T::LocalAssetId, T::ForeignAssetId), CandidateStatus, OptionQuery>;

	#[pallet::event]
	#[pallet::generate_deposit(pub(super) fn deposit_event)]
	pub enum Event<T: Config> {
		LocalAdminUpdated(T::AccountId),
		ForeignAdminUpdated(T::AccountId),
	}

	#[pallet::error]
	pub enum Error<T> {
		OnlyAllowedToAdmins,
		AssetsMappingCandidateNotFound,
	}

	#[pallet::call]
	impl<T: Config> Pallet<T> {
		#[pallet::weight(10_000)]
		pub fn set_local_admin(origin: OriginFor<T>, local_admin: T::AccountId) -> DispatchResultWithPostInfo {
			T::UpdateAdmin::ensure_origin(origin)?;
			<LocalAdmin<T>>::put(local_admin.clone());
			Self::deposit_event(Event::LocalAdminUpdated(local_admin));
			Ok(().into())
		}

		#[pallet::weight(10_000)]
		pub fn set_foreign_admin(origin: OriginFor<T>, foreign_admin: T::AccountId) -> DispatchResultWithPostInfo {
			T::UpdateAdmin::ensure_origin(origin)?;
			<ForeignAdmin<T>>::put(foreign_admin.clone());
			Self::deposit_event(Event::ForeignAdminUpdated(foreign_admin));
			Ok(().into())
		}

		#[pallet::weight(10_000)]
		pub fn approve_assets_mapping_candidate(
			origin: OriginFor<T>,
			local_asset_id: T::LocalAssetId,
			foreign_asset_id: T::ForeignAssetId,
		) -> DispatchResultWithPostInfo {
			let who = ensure_signed(origin.clone())?;
			Self::ensure_admins_only(origin)?;
			Self::approve_candidate(who, local_asset_id, foreign_asset_id)?;
			Ok(().into())
		}
	}

	impl<T: Config> Pallet<T> {
		fn ensure_admins_only(origin: OriginFor<T>) -> DispatchResultWithPostInfo {
			if let (Err(_), Err(_)) = (T::LocalAdminOrigin::ensure_origin(origin.clone()), T::ForeignAdminOrigin::ensure_origin(origin)) {
				Err(Error::<T>::OnlyAllowedToAdmins.into())
			} else {
				Ok(().into())
			}
		}

		fn approve_candidate(
			who: T::AccountId,
			local_asset_id: T::LocalAssetId,
			foreign_asset_id: T::ForeignAssetId
		) -> DispatchResultWithPostInfo {
			let current_candidate_status = <AssetsMappingCandidates<T>>::get((local_asset_id, foreign_asset_id))
				.ok_or(Error::<T>::AssetsMappingCandidateNotFound)?;
			let local_admin = <LocalAdmin<T>>::get();
			let foreign_admin = <ForeignAdmin<T>>::get();
			match current_candidate_status {
				CandidateStatus::NobodyApproved => {
					if who == local_admin {
						<AssetsMappingCandidates<T>>::insert((local_asset_id, foreign_asset_id), CandidateStatus::LocalAdminApproved);
					} else {
						<AssetsMappingCandidates<T>>::insert((local_asset_id, foreign_asset_id), CandidateStatus::LocalAdminApproved);
					}
				},
				CandidateStatus::LocalAdminApproved => {
					if who == foreign_admin {
						<LocalAsset<T>>::insert(local_asset_id, foreign_asset_id);
						<ForeignAsset<T>>::insert(foreign_asset_id, local_asset_id);
						<AssetsMappingCandidates<T>>::remove((local_asset_id, foreign_asset_id));
					}
				},
				CandidateStatus::ForeignAdminApproved => {
					if who == local_admin {
						<LocalAsset<T>>::insert(local_asset_id, foreign_asset_id);
						<ForeignAsset<T>>::insert(foreign_asset_id, local_asset_id);
						<AssetsMappingCandidates<T>>::remove((local_asset_id, foreign_asset_id));
					}
				}
			}; 
			Ok(().into())
		}
	}

	pub struct EnsureLocalAdmin<T>(PhantomData<T>);
	impl<T: Config> EnsureOrigin<T::Origin> for EnsureLocalAdmin<T> {
		type Success = T::AccountId;
		fn try_origin(o: T::Origin) -> Result<Self::Success, T::Origin> {
			o.into().and_then(|o| match (o, LocalAdmin::<T>::try_get()) {
				(frame_system::RawOrigin::Signed(ref who), Ok(ref f)) if who == f => Ok(who.clone()),
				(r, _) => Err(T::Origin::from(r)),
			})
		}    
	
		#[cfg(feature = "runtime-benchmarks")]
		fn successful_origin() -> T::Origin {
			let local_admin = LocalAdmin::<T>::try_get().expect("local admin should exist");
			T::Origin::from(system::RawOrigin::Signed(founder))
		}    
	}

	pub struct EnsureForeignAdmin<T>(PhantomData<T>);
	impl<T: Config> EnsureOrigin<T::Origin> for EnsureForeignAdmin<T> {
		type Success = T::AccountId;
		fn try_origin(o: T::Origin) -> Result<Self::Success, T::Origin> {
			o.into().and_then(|o| match (o, ForeignAdmin::<T>::try_get()) {
				(frame_system::RawOrigin::Signed(ref who), Ok(ref f)) if who == f => Ok(who.clone()),
				(r, _) => Err(T::Origin::from(r)),
			})
		}    
	
		#[cfg(feature = "runtime-benchmarks")]
		fn successful_origin() -> T::Origin {
			let local_admin = ForeignAdmin::<T>::try_get().expect("foreign admin should exist");
			T::Origin::from(system::RawOrigin::Signed(founder))
		}    
	}
}
