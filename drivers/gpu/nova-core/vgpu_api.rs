// SPDX-License-Identifier: GPL-2.0

//! VF registration data for the NVIDIA vGPU VFIO variant driver.
//!
//! Instead of exporting C symbols (`nvidia_vgpu_open`, `nvidia_vgpu_close`,
//! `nvidia_vgpu_reset`), nova-core registers a [`NovaCoreVfApi`] through the
//! PCI VF registration data mechanism. The VF driver retrieves it via
//! [`pci::Device::vf_registration_data_with()`].

use core::pin::Pin;

use kernel::{
    device,
    io::resource,
    pci,
    prelude::*,
    types::ForLt, //
};

use crate::gpu::Gpu;

/// vGPU type descriptor available through [`VgpuInstance::type_info()`].
///
/// Contains the PCI IDs and BAR1 size that the VFIO driver presents to the
/// guest.
#[derive(Debug, Clone, Copy, Default)]
pub struct VgpuTypeInfo {
    /// PCI device ID to present to the guest.
    pub pci_dev_id: u32,
    /// PCI subsystem ID to present to the guest.
    pub pci_subsys_id: u32,
    /// BAR1 aperture size in MiB.
    pub bar1_length: u64,
}

/// API handle for the VFIO variant driver to interact with nova-core.
///
/// This type is registered by the PF driver as VF registration data and
/// accessed by the nvidia-vgpu VF driver through
/// [`pci::Device::vf_registration_data_with()`].
///
/// The lifetime `'a` is tied to the PCI binding scope of the PF. The
/// enclosing [`pci::VfRegistration`] calls `pci_disable_sriov()` in its
/// drop, which blocks until all VF drivers have completed their `remove()`
/// callbacks, so this data is guaranteed valid for the VF's entire lifetime.
pub struct NovaCoreVfApi<'a> {
    pub(crate) pdev: &'a pci::Device<device::Bound>,
    pub(crate) _gpu: &'a Gpu<'a>,
}

impl NovaCoreVfApi<'_> {
    /// Obtain a [`NovaCoreVfApiHandle`] from a VF registered by nova-core.
    pub fn handle(vf: &pci::Device<device::Bound>) -> Result<NovaCoreVfApiHandle<'_>> {
        NovaCoreVfApiHandle::of(vf)
    }

    /// Returns the framebuffer BAR index for the given VF.
    pub fn fb_bar_index(&self, vf: &pci::Device<device::Bound>) -> Result<u32> {
        // A 64-bit BAR0 occupies two slots, moving the framebuffer to BAR2.
        Ok(
            if vf
                .resource_flags(0)?
                .contains(resource::Flags::IORESOURCE_MEM_64)
            {
                2
            } else {
                1
            },
        )
    }
}

/// Closure-based handle to the nova-core VF API.
pub struct NovaCoreVfApiHandle<'a> {
    vf: &'a pci::Device<device::Bound>,
}

impl<'a> NovaCoreVfApiHandle<'a> {
    fn of(vf: &'a pci::Device<device::Bound>) -> Result<Self> {
        vf.vf_registration_data_with::<ForLt!(NovaCoreVfApi<'_>), ()>(|_| ())?;
        Ok(Self { vf })
    }

    /// Activate a vGPU instance, which is closed on drop.
    ///
    /// `gfid` is the VF index + 1, `dbdf` includes the PCI domain, and `vm_pid`
    /// is the thread group ID of the VM process.
    pub fn open(&self, gfid: u32, dbdf: u32, vm_pid: u32) -> Result<VgpuInstance<'a>> {
        VgpuInstance::new(Self { vf: self.vf }, gfid, dbdf, vm_pid)
    }

    /// Access the [`NovaCoreVfApi`] through a closure.
    pub fn with<R>(&self, f: impl for<'b> FnOnce(Pin<&NovaCoreVfApi<'b>>) -> R) -> R {
        self.vf
            .vf_registration_data_with::<ForLt!(NovaCoreVfApi<'_>), R>(f)
            .expect("TypeId was validated in NovaCoreVfApiHandle::of()")
    }
}

/// An active vGPU instance, closed on drop while the VF binding is still valid.
pub struct VgpuInstance<'a> {
    api: NovaCoreVfApiHandle<'a>,
    gfid: u32,
    type_info: VgpuTypeInfo,
}

impl<'a> VgpuInstance<'a> {
    fn new(api: NovaCoreVfApiHandle<'a>, gfid: u32, dbdf: u32, vm_pid: u32) -> Result<Self> {
        let type_info = api.with(|api| {
            dev_dbg!(
                api.pdev,
                "vgpu instance create: gfid={} dbdf={:#x} vm_pid={}\n",
                gfid,
                dbdf,
                vm_pid,
            );

            // TODO: Query the assigned VF type and its properties, then create
            // and activate the instance via VgpuManager.
            Err::<VgpuTypeInfo, Error>(ENOSYS)
        })?;

        Ok(Self {
            api,
            gfid,
            type_info,
        })
    }

    /// Returns the firmware-selected PCI IDs and BAR1 size.
    pub fn type_info(&self) -> &VgpuTypeInfo {
        &self.type_info
    }

    /// Reset this vGPU instance.
    pub fn reset(&self) -> Result {
        self.api.with(|api| {
            dev_dbg!(api.pdev, "vgpu instance reset: gfid={}\n", self.gfid);

            // TODO: Delegate to VgpuManager::instances().reset_instance().
            Err(ENOSYS)
        })
    }
}

impl Drop for VgpuInstance<'_> {
    fn drop(&mut self) {
        self.api.with(|api| {
            dev_dbg!(api.pdev, "vgpu instance destroy: gfid={}\n", self.gfid);

            // TODO: Delegate to VgpuManager::instances().destroy_instance().
        });
    }
}
