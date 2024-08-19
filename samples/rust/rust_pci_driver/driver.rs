// SPDX-License-Identifier: GPL-2.0

//! Rust PCI driver sample

use kernel::{bindings, pci, prelude::*};

const PCI_DEVICE_ID_REDHAT_QEMU_PCI_TESTDEV: u32 = 0x0005;

pub(crate) struct Driver;

impl Driver {
    const IDS: usize = 2;
    const __ID_TABLE: [bindings::pci_device_id; Self::IDS] = Self::id_table();

    const fn id_table() -> [bindings::pci_device_id; Self::IDS] {
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
}

impl pci::Driver for Driver {
    const ID_TABLE: *const bindings::pci_device_id = Self::__ID_TABLE.as_ptr();

    fn probe(_pdev: *mut bindings::pci_dev) -> Result {
        pr_info!("Probe Rust PCI driver sample.\n");

        Ok(())
    }

    fn remove(_pdev: *mut bindings::pci_dev) {
        pr_info!("Remove Rust PCI driver sample.\n");
    }
}
