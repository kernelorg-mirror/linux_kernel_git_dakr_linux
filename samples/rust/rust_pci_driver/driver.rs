// SPDX-License-Identifier: GPL-2.0

//! Rust PCI driver sample

use kernel::{bindings, pci, pci::define_pci_id_table, prelude::*};

const PCI_DEVICE_ID_REDHAT_QEMU_PCI_TESTDEV: u32 = 0x0005;

pub(crate) struct Driver;

impl pci::Driver for Driver {
    define_pci_id_table! {
        (),
        [ (pci::DeviceId::new(bindings::PCI_VENDOR_ID_REDHAT,
                             PCI_DEVICE_ID_REDHAT_QEMU_PCI_TESTDEV), None) ]
    }

    fn probe(_pdev: *mut bindings::pci_dev, id: Option<&Self::IdInfo>) -> Result {
        pr_info!("Probe Rust PCI driver sample.\n");
        pr_info!("Info: {:?}\n", id);

        Ok(())
    }

    fn remove(_pdev: *mut bindings::pci_dev) {
        pr_info!("Remove Rust PCI driver sample.\n");
    }
}
