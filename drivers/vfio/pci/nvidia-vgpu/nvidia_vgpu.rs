// SPDX-License-Identifier: GPL-2.0
// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.

//! NVIDIA vGPU VFIO variant driver (Rust).
//!
//! Binds to NVIDIA VF devices via `driver_override` and coordinates vGPU lifecycle with the
//! nova-core PF driver through the PCI VF registration data mechanism.

use kernel::{
    bindings,
    device::{
        Bound,
        Core, //
    },
    pci,
    pci::{
        Class,
        ClassMask,
        Vendor, //
    },
    prelude::*,
    sync::aref::ARef,
    vfio::{
        self,
        pci::{
            GetRegionInfo,
            Ioctl,
            Read, //
        },
    }, //
};

use nova_core::vgpu_api::{
    NovaCoreVfApi,
    NovaCoreVfApiHandle,
    VgpuInstance, //
};

struct NvidiaVgpuOps;

#[pin_data]
struct NvidiaVgpuOpenData<'a> {
    instance: VgpuInstance<'a>,
}

impl vfio::pci::Operations for NvidiaVgpuOps {
    const NAME: &'static CStr = c"nvidia-vgpu-vfio-pci";
    type RegistrationData<'a> = NvidiaVgpuRegData<'a>;
    type OpenData<'a> = NvidiaVgpuOpenData<'a>;

    fn open_device<'a>(
        _dev: &'a vfio::pci::Device<Self>,
        rd: &'a Self::RegistrationData<'a>,
    ) -> impl PinInit<Self::OpenData<'a>, Error> + 'a {
        try_pin_init!(NvidiaVgpuOpenData {
            instance: {
                let pci_dev = rd.pdev;
                let dbdf = (pci_dev.domain_nr() << 16) | u32::from(pci_dev.dev_id());
                let vm_pid = kernel::current!().tgid().try_into()?;
                let instance = rd.api.open(rd.gfid, dbdf, vm_pid)?;
                let type_info = instance.type_info();

                dev_dbg!(
                    pci_dev,
                    "vgpu open: gfid={} dev_id={:#x} subsys_id={:#x} bar1={:#x}\n",
                    rd.gfid,
                    type_info.pci_dev_id,
                    type_info.pci_subsys_id,
                    type_info.bar1_length,
                );

                instance
            },
        })
    }

    fn ioctl<'a>(
        dev: &vfio::pci::Device<Self, Ioctl>,
        _rd: &Self::RegistrationData<'a>,
        open_data: Pin<&Self::OpenData<'a>>,
        cmd: u32,
        arg: usize,
    ) -> Result<isize> {
        if cmd == vfio::DEVICE_RESET {
            open_data.instance.reset()?;
        }

        dev.core_ioctl(cmd, arg)
    }

    fn read<'a>(
        dev: &vfio::pci::Device<Self, Read>,
        _rd: &Self::RegistrationData<'a>,
        open_data: Pin<&Self::OpenData<'a>>,
        buf: &mut vfio::UserBuf,
        ppos: &mut vfio::pci::Position<'_>,
    ) -> Result<isize> {
        if ppos.region_index() != vfio::pci::CONFIG_REGION_INDEX {
            return dev.core_read(buf, ppos);
        }

        let pos = ppos.region_offset();
        let ret = dev.core_read(buf, ppos)?;
        let read_len = ret.try_into()?;
        let type_info = open_data.instance.type_info();

        buf.write_overlapping(
            pos,
            read_len,
            u64::from(bindings::PCI_DEVICE_ID),
            &(type_info.pci_dev_id as u16).to_le_bytes(),
        )?;

        buf.write_overlapping(
            pos,
            read_len,
            u64::from(bindings::PCI_SUBSYSTEM_ID),
            &(type_info.pci_subsys_id as u16).to_le_bytes(),
        )?;

        Ok(ret)
    }

    fn get_region_info<'a>(
        dev: &vfio::pci::Device<Self, GetRegionInfo>,
        rd: &Self::RegistrationData<'a>,
        open_data: Pin<&Self::OpenData<'a>>,
        info: &mut bindings::vfio_region_info,
        caps: &mut vfio::InfoCap<'_>,
    ) -> Result {
        dev.core_get_region_info(info, caps)?;

        let fb_bar = rd.api.with(|api| api.fb_bar_index(rd.pdev))?;
        if info.index == fb_bar && info.size != 0 {
            let bar1_mib = open_data.instance.type_info().bar1_length;
            let bar1_size = bar1_mib.checked_mul(1 << 20).ok_or(EOVERFLOW)?;
            if bar1_size != 0 {
                info.size = info.size.min(bar1_size);
            }
        }

        Ok(())
    }
}

struct NvidiaVgpuDriver;

kernel::pci_device_table!(
    PCI_TABLE,
    <NvidiaVgpuDriver as pci::Driver>::IdInfo,
    [
        (
            pci::DeviceId::from_class_and_vendor_vfio_override(
                Class::DISPLAY_VGA,
                ClassMask::ClassSubclass,
                Vendor::NVIDIA
            ),
            ()
        ),
        (
            pci::DeviceId::from_class_and_vendor_vfio_override(
                Class::DISPLAY_3D,
                ClassMask::ClassSubclass,
                Vendor::NVIDIA
            ),
            ()
        ),
    ]
);

#[pin_data]
struct NvidiaVgpuData<'bound> {
    _vdev: ARef<vfio::pci::Device<NvidiaVgpuOps>>,
    _reg: vfio::pci::Registration<'bound, NvidiaVgpuOps>,
}

/// Per-device registration data for the nvidia-vgpu variant driver.
///
/// Holds the PCI device, nova-core API handle, and Guest Function ID.
#[pin_data]
pub struct NvidiaVgpuRegData<'a> {
    pdev: &'a pci::Device<Bound>,
    api: NovaCoreVfApiHandle<'a>,
    gfid: u32,
}

impl pci::Driver for NvidiaVgpuDriver {
    type IdInfo = ();
    type Data<'bound> = NvidiaVgpuData<'bound>;
    const ID_TABLE: pci::IdTable<Self::IdInfo> = &PCI_TABLE;
    const DRIVER_MANAGED_DMA: bool = true;

    fn probe<'bound>(
        pdev: &'bound pci::Device<Core<'_>>,
        _info: Option<&'bound Self::IdInfo>,
    ) -> impl PinInit<Self::Data<'bound>, Error> + 'bound {
        try_pin_init!(Self::Data {
            _: {
                if !pdev.is_virtfn() {
                    return Err(ENODEV);
                }
            },

            _vdev: vfio::pci::Device::<NvidiaVgpuOps>::new(pdev)?,

            // SAFETY: The registration is dropped when the PCI driver is unbound.
            _reg <- unsafe { vfio::pci::Registration::new(
                pdev,
                _vdev,
                try_pin_init!(NvidiaVgpuRegData {
                    pdev,
                    api: NovaCoreVfApi::handle(pdev)?,
                    gfid: pdev.vf_id()? + 1,
                }),
            )},
        })
    }
}

kernel::module_pci_driver! {
    type: NvidiaVgpuDriver,
    name: "nvidia-vgpu-vfio-pci",
    authors: ["NVIDIA"],
    description: "NVIDIA vGPU VFIO variant driver",
    license: "GPL v2",
}
