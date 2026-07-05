// SPDX-License-Identifier: GPL-2.0

//! Nova Core API for auxiliary bus child drivers.

use core::pin::Pin;

use kernel::{
    auxiliary,
    device::Bound,
    prelude::*,
    types::{
        CovariantForLt,
        ForLt, //
    },
};

use crate::gpu::{
    Chipset,
    Gpu, //
};

/// API handle for auxiliary bus child drivers to interact with nova-core.
pub struct NovaCoreApi<'a> {
    pub(crate) gpu: Pin<&'a Gpu<'a>>,
}

impl NovaCoreApi<'_> {
    /// Obtain a [`NovaCoreApi`] handle from an auxiliary device registered by nova-core.
    pub fn of(adev: &auxiliary::Device<Bound>) -> Result<Pin<&NovaCoreApi<'_>>> {
        adev.registration_data::<CovariantForLt!(NovaCoreApi<'_>)>()
    }

    /// Like [`NovaCoreApi::of`], but returns a handle for invariant registration data types.
    pub fn handle(adev: &auxiliary::Device<Bound>) -> Result<NovaCoreApiHandle<'_>> {
        NovaCoreApiHandle::of(adev)
    }

    /// Returns the chipset of this GPU.
    pub fn chipset(&self) -> Chipset {
        self.gpu.spec.chipset
    }

    /// Returns the total usable VRAM size of this GPU in bytes.
    pub fn vram_size(&self) -> u64 {
        self.gpu.gsp_static_info.vram_size()
    }
}

/// Closure-based API handle for invariant registration data types.
pub struct NovaCoreApiHandle<'a> {
    adev: &'a auxiliary::Device<Bound>,
}

impl<'a> NovaCoreApiHandle<'a> {
    fn of(adev: &'a auxiliary::Device<Bound>) -> Result<Self> {
        adev.registration_data_with::<ForLt!(NovaCoreApi<'_>), ()>(|_| ())?;
        Ok(Self { adev })
    }

    /// Access the [`NovaCoreApi`] through a closure.
    pub fn with<R>(&self, f: impl for<'b> FnOnce(Pin<&'b NovaCoreApi<'b>>) -> R) -> R {
        // PANIC: TypeId was validated in `of()`, cannot fail.
        self.adev
            .registration_data_with::<ForLt!(NovaCoreApi<'_>), R>(f)
            .unwrap()
    }
}
