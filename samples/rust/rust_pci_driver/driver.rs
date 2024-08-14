// SPDX-License-Identifier: GPL-2.0

//! Rust PCI driver sample

use kernel::{bindings, prelude::*};

const __LOG_PREFIX: &[u8] = b"rust_pci_driver_sample\0";

#[no_mangle]
unsafe extern "C" fn rust_pci_driver_probe(
    _pdev: *mut bindings::pci_dev,
    _ent: bindings::pci_device_id,
) -> core::ffi::c_int {
    pr_info!("Probe Rust PCI driver sample.\n");

    0
}

#[no_mangle]
unsafe extern "C" fn rust_pci_driver_remove(_pdev: *mut bindings::pci_dev) {
    pr_info!("Remove Rust PCI driver sample.\n");
}
