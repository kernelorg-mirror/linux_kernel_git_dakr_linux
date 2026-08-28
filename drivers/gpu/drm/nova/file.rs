// SPDX-License-Identifier: GPL-2.0

use crate::driver::{
    DrmRegData,
    NovaDevice,
    NovaDriver, //
};
use crate::gem::NovaObject;
use kernel::{
    alloc::flags::*,
    drm::{
        self,
        gem::BaseObject,
        Registered, //
    },
    prelude::*,
    transmute::AsBytes,
    uaccess::UserSlice,
    uapi,
};

pub(crate) struct File;

/// GPU information returned to userspace.
///
/// # Invariants
///
/// - The layout of this type is identical to `struct drm_nova_gpu_info`.
/// - All bytes in the value are initialized.
#[repr(transparent)]
struct GpuInfo(uapi::drm_nova_gpu_info);

impl GpuInfo {
    fn new(reg_data: &DrmRegData<'_>) -> Self {
        Self(uapi::drm_nova_gpu_info {
            architecture: reg_data.api.architecture(),
            implementation: reg_data.api.implementation(),
            vram_size: reg_data.api.vram_size(),
            gpu_name: reg_data.api.gpu_name(),
            gpu_short_name: reg_data.api.gpu_short_name(),
        })
    }
}

// SAFETY: `GpuInfo` has no implicit padding, kernel pointers, or interior
// mutability, and all of its fields are initialized before it is written to
// userspace.
unsafe impl AsBytes for GpuInfo {}

fn write_info<T: AsBytes>(info: &mut uapi::drm_nova_info, value: &T) -> Result {
    let mut writer =
        UserSlice::new(UserPtr::from_addr(info.data as usize), info.size as usize).writer();

    info.size = writer.write_truncated(value)? as u32;

    Ok(())
}

impl drm::file::DriverFile for File {
    type Driver = NovaDriver;

    fn open(_dev: &NovaDevice) -> Result<Pin<KBox<Self>>> {
        Ok(KBox::new(Self, GFP_KERNEL)?.into())
    }
}

impl File {
    /// IOCTL: get_param: Query GPU / driver metadata.
    pub(crate) fn get_param(
        _dev: &NovaDevice<Registered>,
        reg_data: &DrmRegData<'_>,
        getparam: &mut uapi::drm_nova_getparam,
        _file: &drm::File<File>,
    ) -> Result<u32> {
        let value = match getparam.param as u32 {
            uapi::NOVA_GETPARAM_VRAM_BAR_SIZE => reg_data.api.bar1_size()?,
            _ => return Err(EINVAL),
        };

        getparam.value = Into::<u64>::into(value);

        Ok(0)
    }

    /// IOCTL: gem_create: Create a new DRM GEM object.
    pub(crate) fn gem_create(
        dev: &NovaDevice<Registered>,
        _reg_data: &DrmRegData<'_>,
        req: &mut uapi::drm_nova_gem_create,
        file: &drm::File<File>,
    ) -> Result<u32> {
        let obj = NovaObject::new(dev, req.size.try_into()?)?;

        req.handle = obj.create_handle(file)?;

        Ok(0)
    }

    /// IOCTL: gem_info: Query GEM metadata.
    pub(crate) fn gem_info(
        _dev: &NovaDevice<Registered>,
        _reg_data: &DrmRegData<'_>,
        req: &mut uapi::drm_nova_gem_info,
        file: &drm::File<File>,
    ) -> Result<u32> {
        let bo = NovaObject::lookup_handle(file, req.handle)?;

        req.size = bo.size().try_into()?;

        Ok(0)
    }

    /// IOCTL: info: Query device information.
    pub(crate) fn info(
        _dev: &NovaDevice<Registered>,
        reg_data: &DrmRegData<'_>,
        info: &mut uapi::drm_nova_info,
        _file: &drm::File<File>,
    ) -> Result<u32> {
        if info.data == 0 {
            info.size = match info.id {
                uapi::DRM_NOVA_INFO_GPU => size_of::<GpuInfo>() as u32,
                _ => return Err(EINVAL),
            };
            return Ok(0);
        }

        match info.id {
            uapi::DRM_NOVA_INFO_GPU => write_info(info, &GpuInfo::new(reg_data))?,
            _ => return Err(EINVAL),
        }

        Ok(0)
    }
}
