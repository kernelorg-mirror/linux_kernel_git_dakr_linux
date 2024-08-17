// SPDX-License-Identifier: GPL-2.0

//! Rust PCI driver sample

use kernel::{bindings, c_str, prelude::*};

const PCI_DEVICE_ID_REDHAT_QEMU_PCI_TESTDEV: u32 = 0x0005;

pub(crate) static mut DRIVER: bindings::pci_driver = Driver::driver();

struct Driver;

impl Driver {
    const IDS: usize = 2;
    const __ID_TABLE: [bindings::pci_device_id; Self::IDS] = Self::id_table();

    const fn driver() -> bindings::pci_driver {
        // SAFETY: `bindings::pci_driver` is valid to be zero initialized.
        let mut drv: bindings::pci_driver = unsafe { core::mem::zeroed() };

        drv.name = c_str!("rust_pci_driver_sample").as_char_ptr();
        drv.id_table = Self::__ID_TABLE.as_ptr();
        drv.probe = Some(Self::probe);
        drv.remove = Some(Self::remove);

        drv
    }

    const fn id_table() -> [bindings::pci_device_id; 2] {
        // SAFETY: `bindings::pci_device_id` is valid to be zero initialized.
        let mut id: bindings::pci_device_id = unsafe { core::mem::zeroed() };

        id.vendor = bindings::PCI_VENDOR_ID_REDHAT;
        id.device = PCI_DEVICE_ID_REDHAT_QEMU_PCI_TESTDEV;
        id.subvendor = bindings::PCI_ANY_ID as u32;
        id.subdevice = bindings::PCI_ANY_ID as u32;

        // SAFETY: `bindings::pci_device_id` is valid to be zero initialized.
        let sentinel: bindings::pci_device_id = unsafe { core::mem::zeroed() };

        [id, sentinel]
    }

    extern "C" fn probe(
        _pdev: *mut bindings::pci_dev,
        _ent: *const bindings::pci_device_id,
    ) -> core::ffi::c_int {
        pr_info!("Probe Rust PCI driver sample.\n");

        0
    }

    extern "C" fn remove(_pdev: *mut bindings::pci_dev) {
        pr_info!("Remove Rust PCI driver sample.\n");
    }
}
