// SPDX-License-Identifier: GPL-2.0

//! Wrappers for the PCI subsystem
//!
//! C header: [`include/linux/pci.h`](srctree/include/linux/pci.h)

use core::marker::PhantomData;
use kernel::{
    bindings, driver,
    error::{from_result, to_result},
    prelude::*,
};

/// Abstraction for `bindings::pci_device_id`.
#[derive(Clone, Copy)]
pub struct DeviceId {
    /// Vendor ID
    pub vendor: u32,
    /// Device ID
    pub device: u32,
    /// Subsystem vendor ID
    pub subvendor: u32,
    /// Subsystem device ID
    pub subdevice: u32,
    /// Device class and subclass
    pub class: u32,
    /// Limit which sub-fields of the class
    pub class_mask: u32,
}

impl DeviceId {
    /// Zeroed `bindings::pci_device_id`.
    // SAFETY; The all-zero byte-pattern is valid for `bindings::pci_device_id`.
    pub const ZERO: bindings::pci_device_id = unsafe { core::mem::zeroed() };
    const PCI_ANY_ID: u32 = !0;

    /// Equivalent to the PCI_DEVICE macro.
    pub const fn new(vendor: u32, device: u32) -> Self {
        Self {
            vendor,
            device,
            subvendor: DeviceId::PCI_ANY_ID,
            subdevice: DeviceId::PCI_ANY_ID,
            class: 0,
            class_mask: 0,
        }
    }

    /// Convert `DeviceId` to raw `bindings::pci_device_id`.
    ///
    /// `offset` is the offset of `ids[i]` to `id_infos[i]` within `IdArray`.
    pub const fn to_rawid(&self, offset: usize) -> bindings::pci_device_id {
        let mut raw = Self::ZERO;

        raw.vendor = self.vendor;
        raw.device = self.device;
        raw.subvendor = self.subvendor;
        raw.subdevice = self.subdevice;
        raw.class = self.class;
        raw.class_mask = self.class_mask;
        raw.driver_data = offset as _;

        raw
    }
}

/// A zero-terminated PCI device ID array.
#[repr(C)]
pub struct IdArray<U, const N: usize> {
    ids: [bindings::pci_device_id; N],
    sentinel: bindings::pci_device_id,
    id_infos: [Option<U>; N],
}

impl<U, const N: usize> IdArray<U, N> {
    /// Creates a new instance of the ID array.
    ///
    /// The contents are derived from the given identifiers.
    #[doc(hidden)]
    pub const fn new(ids: [bindings::pci_device_id; N], infos: [Option<U>; N]) -> Self {
        Self {
            ids,
            sentinel: DeviceId::ZERO,
            id_infos: infos,
        }
    }

    /// Returns an `IdTable` backed by `self`.
    ///
    /// This is used to essentially erase the array size.
    pub const fn as_table(&self) -> IdTable<'_, U> {
        IdTable {
            first: &self.ids[0],
            _p: PhantomData,
        }
    }

    /// Returns the offset of `ids[i]` to `id_infos[i]` within `IdArray`.
    #[doc(hidden)]
    pub const fn get_offset(index: usize) -> usize {
        let id_size = core::mem::size_of::<bindings::pci_device_id>();
        let info_size = core::mem::size_of::<Option<U>>();

        id_size * (N - index + 1) + info_size * index
    }
}

/// A device ID table.
///
/// The table is guaranteed to be zero-terminated.
#[repr(C)]
pub struct IdTable<'a, U> {
    first: &'a bindings::pci_device_id,
    _p: PhantomData<&'a U>,
}

impl<U> AsRef<bindings::pci_device_id> for IdTable<'_, U> {
    fn as_ref(&self) -> &bindings::pci_device_id {
        self.first
    }
}

/// Converts a comma-separated list of pairs into an array with the first element. That is, it
/// discards the second element of the pair.
///
/// Additionally, it automatically introduces a type if the first element is warpped in curly
/// braces, for example, if it's `{v: 10}`, it becomes `X { v: 10 }`; this is to avoid repeating
/// the type.
#[macro_export]
macro_rules! first_item {
    ($id_type:ty, $(({$($first:tt)*}, $second:expr)),* $(,)?) => {
        {
            type IdType = $id_type;
            [$(IdType{$($first)*},)*]
        }
    };
    ($id_type:ty, $(($first:expr, $second:expr)),* $(,)?) => { [$($first,)*] };
}

/// Converts a comma-separated list of pairs into an array with the second element. That is, it
/// discards the first element of the pair.
#[macro_export]
macro_rules! second_item {
    ($(({$($first:tt)*}, $second:expr)),* $(,)?) => { [$($second,)*] };
    ($(($first:expr, $second:expr)),* $(,)?) => { [$($second,)*] };
}

/// Counts the number of parenthesis-delimited, comma-separated items.
#[macro_export]
macro_rules! count_paren_items {
    (($($item:tt)*), $($remaining:tt)*) => { 1 + $crate::count_paren_items!($($remaining)*) };
    (($($item:tt)*)) => { 1 };
    () => { 0 };
}

#[macro_export]
#[doc(hidden)]
macro_rules! define_pci_id_array {
    ($id_type:ty, $($args:tt)*) => {{
        const fn new<U, const N: usize>(
            ids: [$crate::pci::DeviceId; N],
            infos: [Option<U>; N]) -> $crate::pci::IdArray<U, N>
        {
            let mut raw_ids = [$crate::pci::DeviceId::ZERO; N];

            // The offset of `ids[i]` to `id_infos[i]` within `IdArray`.

            let mut i = 0usize;
            while i < N {
                let offset = $crate::pci::IdArray::<U, N>::get_offset(i);

                raw_ids[i] = ids[i].to_rawid(offset);
                i += 1;
            }

            $crate::pci::IdArray::<U, N>::new(raw_ids, infos)
        }

        new($crate::first_item!($id_type, $($args)*), $crate::second_item!($($args)*))
    }}
}

/// Define a const PCI device ID table.
#[macro_export]
macro_rules! define_pci_id_table {
    ($id_type:ty, [ $($args:tt)* ]) => {
        type IdInfo = $id_type;
        const ID_TABLE: $crate::pci::IdTable<'static, $id_type> = {
            const ARRAY: $crate::pci::IdArray<$id_type, { $crate::count_paren_items!($($args)*) }> =
                $crate::define_pci_id_array!($id_type, $($args)*);
            ARRAY.as_table()
        };
    };
}
pub use define_pci_id_table;

/// Drivers must implement this trait to register a PCI driver.
pub trait Driver {
    /// The type holding information about each device ID supported by the drivID.
    type IdInfo: 'static;

    /// The table of device IDs supported by this driver.
    const ID_TABLE: IdTable<'static, Self::IdInfo>;

    /// PCI driver probe.
    ///
    /// Called when a PCI device is matched against a PCI driver.
    fn probe(pdev: *mut bindings::pci_dev, id: Option<&Self::IdInfo>) -> Result;

    /// PCI driver remove.
    ///
    /// Called when the PCI device is unbound.
    fn remove(pdev: *mut bindings::pci_dev);
}

/// PCI abstraction for registering PCI drivers.
pub struct Adapter<T: Driver>(PhantomData<T>);

impl<T> Adapter<T>
where
    T: Driver,
{
    extern "C" fn probe(
        pdev: *mut bindings::pci_dev,
        id: *const bindings::pci_device_id,
    ) -> core::ffi::c_int {
        // SAFETY: `id` is a pointer within the static table, so it's always valid.
        let offset = unsafe { (*id).driver_data };

        let info = {
            // SAFETY: The offset comes from a previous call to `offset_from` in `IdArray::new`,
            // which guarantees that the resulting pointer is within the table.
            let ptr = unsafe {
                id.cast::<u8>()
                    .offset(offset as _)
                    .cast::<Option<T::IdInfo>>()
            };

            // SAFETY: Guaranteed by the preceding safety requirement.
            unsafe { (*ptr).as_ref() }
        };

        from_result(|| {
            T::probe(pdev, info)?;
            Ok(0)
        })
    }

    extern "C" fn remove(pdev: *mut bindings::pci_dev) {
        T::remove(pdev);
    }
}

impl<T> driver::RegistrationOps for Adapter<T>
where
    T: Driver,
{
    type RegType = bindings::pci_driver;

    unsafe fn register(
        pdrv: *mut Self::RegType,
        name: &'static CStr,
        module: &'static ThisModule,
    ) -> Result {
        // SAFETY: By the safety requirements of this function `pdrv` is valid; we never move out
        // of `pdrv`.
        let pdrv = unsafe { &mut *pdrv };

        pdrv.name = name.as_char_ptr();
        pdrv.probe = Some(Self::probe);
        pdrv.remove = Some(Self::remove);
        pdrv.id_table = T::ID_TABLE.as_ref();

        // SAFETY: `pdrv` is a valid `struct pci_driver`; `ThisModule` is equivalent to
        // C's `THIS_MODULE` and hence valid for `__pci_register_driver`. `name` is passed as `NULL`
        // terminated C string.
        //
        // Returns zero when the driver was registered successfully, a non-zero error code
        // otherwise, which is handled by `to_result`.
        to_result(unsafe {
            bindings::__pci_register_driver(pdrv, module.as_ptr(), name.as_char_ptr())
        })
    }

    unsafe fn unregister(pdrv: *mut Self::RegType) {
        // SAFETY: `pdrv` is guaranteed to be a valid `RegType`.
        unsafe { bindings::pci_unregister_driver(pdrv) }
    }
}
