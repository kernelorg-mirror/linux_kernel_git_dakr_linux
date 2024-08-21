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

/// Drivers must implement this trait to register a PCI driver.
pub trait Driver {
    /// Pointer to the PCI driver's ID table.
    ///
    /// This is highly unsafe, since we have to trust the driver, that the provided pointer is
    /// valid.
    const ID_TABLE: *const bindings::pci_device_id;

    /// PCI driver probe.
    ///
    /// Called when a PCI device is matched against a PCI driver.
    fn probe(pdev: *mut bindings::pci_dev) -> Result;

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
        _ent: *const bindings::pci_device_id,
    ) -> core::ffi::c_int {
        from_result(|| {
            T::probe(pdev)?;
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
        pdrv.id_table = T::ID_TABLE;

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
