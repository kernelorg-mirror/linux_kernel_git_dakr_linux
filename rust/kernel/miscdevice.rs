// SPDX-License-Identifier: GPL-2.0

// Copyright (C) 2024 Google LLC.

//! Miscdevice support.
//!
//! C headers: [`include/linux/miscdevice.h`](srctree/include/linux/miscdevice.h).
//!
//! Reference: <https://www.kernel.org/doc/html/latest/driver-api/misc_devices.html>

use crate::{
    bindings, container_of,
    device::{Bound, Device},
    devres::Devres,
    error::{to_result, Error, Result, VTABLE_DEFAULT_ERROR},
    ffi::{c_int, c_long, c_uint, c_ulong},
    fs::File,
    prelude::*,
    seq_file::SeqFile,
    str::CStr,
    types::{ARef, Opaque},
};
use core::{marker::PhantomData, mem::MaybeUninit, pin::Pin, ptr::NonNull};

/// Options for creating a misc device.
#[derive(Copy, Clone)]
pub struct MiscDeviceOptions {
    /// The name of the miscdevice.
    pub name: &'static CStr,
}

impl MiscDeviceOptions {
    /// Create a raw `struct miscdev` ready for registration.
    pub const fn into_raw<T: MiscDevice>(self) -> bindings::miscdevice {
        // SAFETY: All zeros is valid for this C type.
        let mut result: bindings::miscdevice = unsafe { MaybeUninit::zeroed().assume_init() };
        result.minor = bindings::MISC_DYNAMIC_MINOR as _;
        result.name = self.name.as_char_ptr();
        result.fops = MiscdeviceVTable::<T>::build();
        result
    }
}

/// # Invariants
///
/// - `inner` is a registered misc device,
/// - `data` is valid for the entire lifetime of `Self`.
#[repr(C)]
#[pin_data(PinnedDrop)]
struct RawDeviceRegistration<T: MiscDevice> {
    #[pin]
    inner: Opaque<bindings::miscdevice>,
    data: NonNull<T::RegistrationData>,
    private: Opaque<T::Ptr>,
    _t: PhantomData<T>,
}

impl<T: MiscDevice> RawDeviceRegistration<T> {
    fn new<'a>(
        opts: MiscDeviceOptions,
        parent: Option<&'a Device<Bound>>,
        data: &'a T::RegistrationData,
    ) -> impl PinInit<Self, Error> + 'a
    where
        T: 'a,
    {
        try_pin_init!(Self {
            // INVARIANT: `Self` is always embedded in a `MiscDeviceRegistration<T>`, hence `data`
            // is guaranteed to be valid for the entire lifetime of `Self`.
            data: NonNull::from(data),
            private: Opaque::uninit(),
            inner <- Opaque::try_ffi_init(move |slot: *mut bindings::miscdevice| {
                let mut value = opts.into_raw::<T>();

                if let Some(parent) = parent {
                    // The device core code will take care to take a reference of `parent` in
                    // `device_add()` called by `misc_register()`.
                    value.parent = parent.as_raw();
                }

                // SAFETY: The initializer can write to the provided `slot`.
                unsafe { slot.write(value) };

                // SAFETY:
                // * We just wrote the misc device options to the slot. The miscdevice will
                //   get unregistered before `slot` is deallocated because the memory is pinned and
                //   the destructor of this type deallocates the memory.
                // * `data` is Initialized before `misc_register` so no race with `fops->open()`
                //   is possible.
                // INVARIANT: If this returns `Ok(())`, then the `slot` will contain a registered
                // misc device.
                to_result(unsafe { bindings::misc_register(slot) })
            }),
            _t: PhantomData,
        })
    }

    /// Returns a raw pointer to the misc device.
    fn as_raw(&self) -> *mut bindings::miscdevice {
        self.inner.get()
    }

    /// Access the `this_device` field.
    fn device(&self) -> &Device {
        // SAFETY: This can only be called after a successful register(), which always
        // initialises `this_device` with a valid device. Furthermore, the signature of this
        // function tells the borrow-checker that the `&Device` reference must not outlive the
        // `&MiscDeviceRegistration<T>` used to obtain it, so the last use of the reference must be
        // before the underlying `struct miscdevice` is destroyed.
        unsafe { Device::as_ref((*self.as_raw()).this_device) }
    }

    fn data(&self) -> &T::RegistrationData {
        // SAFETY: The type invariant guarantees that `data` is valid for the entire lifetime of
        // `Self`.
        unsafe { self.data.as_ref() }
    }
}

#[pinned_drop]
impl<T: MiscDevice> PinnedDrop for RawDeviceRegistration<T> {
    fn drop(self: Pin<&mut Self>) {
        // SAFETY: We know that the device is registered by the type invariants.
        unsafe { bindings::misc_deregister(self.inner.get()) };
    }
}

#[expect(dead_code)]
enum DeviceRegistrationInner<T: MiscDevice> {
    Raw(Pin<KBox<RawDeviceRegistration<T>>>),
    Managed(Devres<RawDeviceRegistration<T>>),
}

/// A registration of a miscdevice.
#[pin_data(PinnedDrop)]
pub struct MiscDeviceRegistration<T: MiscDevice> {
    inner: DeviceRegistrationInner<T>,
    #[pin]
    data: Opaque<T::RegistrationData>,
    this_device: ARef<Device>,
    _t: PhantomData<T>,
}

// SAFETY:
// - It is allowed to call `misc_deregister` on a different thread from where you called
//   `misc_register`.
// - Only implements `Send` if `MiscDevice::RegistrationData` is also `Send`.
unsafe impl<T: MiscDevice> Send for MiscDeviceRegistration<T> where T::RegistrationData: Send {}

// SAFETY:
// - All `&self` methods on this type are written to ensure that it is safe to call them in
//   parallel.
// - `MiscDevice::RegistrationData` is always `Sync`.
unsafe impl<T: MiscDevice> Sync for MiscDeviceRegistration<T> {}

impl<T: MiscDevice> MiscDeviceRegistration<T> {
    /// Register a misc device.
    pub fn register<'a>(
        opts: MiscDeviceOptions,
        data: impl PinInit<T::RegistrationData, Error> + 'a,
        parent: Option<&'a Device<Bound>>,
    ) -> impl PinInit<Self, Error> + 'a
    where
        T: 'a,
    {
        let mut dev: Option<ARef<Device>> = None;

        try_pin_init!(&this in Self {
            data <- Opaque::pin_init(data),
            // TODO: make `inner` in-place when enums get supported by pin-init.
            //
            // Link: https://github.com/Rust-for-Linux/pin-init/issues/59
            inner: {
                // SAFETY:
                //   - `this` is a valid pointer to `Self`,
                //   - `data` was properly initialized above.
                let data = unsafe { &*(*this.as_ptr()).data.get() };

                let raw = RawDeviceRegistration::new(opts, parent, data);

                // FIXME: Work around a bug in rustc, to prevent the following warning:
                //
                //   "warning: value captured by `dev` is never read."
                //
                // Link: https://github.com/rust-lang/rust/issues/141615
                let _ = dev;

                if let Some(parent) = parent {
                    let devres = Devres::new(parent, raw, GFP_KERNEL)?;

                    dev = Some(devres.access(parent)?.device().into());
                    DeviceRegistrationInner::Managed(devres)
                } else {
                    let boxed = KBox::pin_init(raw, GFP_KERNEL)?;

                    dev = Some(boxed.device().into());
                    DeviceRegistrationInner::Raw(boxed)
                }
            },
            // Cache `this_device` within `Self` to avoid having to access `Devres` in the managed
            // case.
            this_device: {
                // SAFETY: `dev` is guaranteed to be set in the initializer of `inner` above.
                unsafe { dev.unwrap_unchecked() }
            },
            _t: PhantomData,
        })
    }

    /// Access the `this_device` field.
    pub fn device(&self) -> &Device {
        &self.this_device
    }

    /// Access the additional data stored in this registration.
    pub fn data(&self) -> &T::RegistrationData {
        // SAFETY:
        // * No mutable reference to the value contained by `self.data` can ever be created.
        // * The value contained by `self.data` is valid for the entire lifetime of `&self`.
        unsafe { &*self.data.get() }
    }
}

#[pinned_drop]
impl<T: MiscDevice> PinnedDrop for MiscDeviceRegistration<T> {
    fn drop(self: Pin<&mut Self>) {
        // SAFETY: `self.data` is valid for dropping.
        unsafe { core::ptr::drop_in_place(self.data.get()) };
    }
}

/// The arguments passed to the file operation callbacks of a [`MiscDeviceRegistration`].
pub struct MiscArgs<'a, T: MiscDevice> {
    /// The [`Device`] representation of the `struct miscdevice`.
    pub device: &'a Device,
    /// The parent [`Device`] of [`Self::device`].
    pub parent: Option<&'a Device<Bound>>,
    /// The `RegistrationData` passed to [`MiscDeviceRegistration::register`].
    pub data: &'a T::RegistrationData,
}

/// Trait implemented by the private data of an open misc device.
#[vtable]
pub trait MiscDevice: Sized {
    /// What kind of pointer should `Self` be wrapped in.
    type Ptr: Send + Sync;

    /// The additional data carried by the [`MiscDeviceRegistration`] for this [`MiscDevice`].
    /// If no additional data is required than the unit type `()` should be used.
    ///
    /// This data can be accessed in [`MiscDevice::open()`].
    type RegistrationData: Sync;

    /// Called when the misc device is opened.
    ///
    /// The returned pointer will be stored as the private data for the file.
    fn open(_file: &File, _args: MiscArgs<'_, Self>) -> Result<Self::Ptr>;

    /// Handler for ioctls.
    ///
    /// The `cmd` argument is usually manipulated using the utilties in [`kernel::ioctl`].
    ///
    /// [`kernel::ioctl`]: mod@crate::ioctl
    fn ioctl(
        _args: MiscArgs<'_, Self>,
        _device: &Self::Ptr,
        _file: &File,
        _cmd: u32,
        _arg: usize,
    ) -> Result<isize> {
        build_error!(VTABLE_DEFAULT_ERROR)
    }

    /// Handler for ioctls.
    ///
    /// Used for 32-bit userspace on 64-bit platforms.
    ///
    /// This method is optional and only needs to be provided if the ioctl relies on structures
    /// that have different layout on 32-bit and 64-bit userspace. If no implementation is
    /// provided, then `compat_ptr_ioctl` will be used instead.
    #[cfg(CONFIG_COMPAT)]
    fn compat_ioctl(
        _args: MiscArgs<'_, Self>,
        _device: &Self::Ptr,
        _file: &File,
        _cmd: u32,
        _arg: usize,
    ) -> Result<isize> {
        build_error!(VTABLE_DEFAULT_ERROR)
    }

    /// Show info for this fd.
    fn show_fdinfo(_args: MiscArgs<'_, Self>, _device: &Self::Ptr, _m: &SeqFile, _file: &File) {
        build_error!(VTABLE_DEFAULT_ERROR)
    }
}

/// A vtable for the file operations of a Rust miscdevice.
struct MiscdeviceVTable<T: MiscDevice>(PhantomData<T>);

impl<T: MiscDevice> MiscdeviceVTable<T> {
    /// # Safety
    ///
    /// This function must only be called from misc device file operations with the `struct file`
    /// pointer provided by the corresponding callback.
    unsafe fn registration_from_file<'a>(
        raw_file: *mut bindings::file,
    ) -> &'a RawDeviceRegistration<T> {
        // SAFETY:
        // * Since `raw_file` comes from a misc device file operation callback, it is a pointer to a
        //   valid `struct file`.
        // * All file operations can access the file's private data.
        let misc_ptr = unsafe { (*raw_file).private_data };

        // This is a miscdevice, so `misc_open()` sets the private data to a pointer to the
        // associated `struct miscdevice` before calling into this method.
        let misc_ptr = misc_ptr.cast::<bindings::miscdevice>();

        // SAFETY:
        // * File operation callbacks ensure that the `struct miscdevice` can't be unregistered and
        //   freed during a call.
        // * The `misc_ptr` always points to the `inner` field of a `RawDeviceRegistration<T>`.
        // * The `RawDeviceRegistration<T>` is valid until the `struct miscdevice` was
        //   unregistered.
        unsafe { &*container_of!(misc_ptr, RawDeviceRegistration<T>, inner) }
    }

    fn args_from_registration<'a>(registration: &'a RawDeviceRegistration<T>) -> MiscArgs<'a, T> {
        let parent: Option<&'a Device<Bound>> = registration.device().parent().map(|parent| {
            // SAFETY: We just convert from `&Device` into `Device<Bound>`.
            unsafe { Device::as_ref(parent.as_raw()) }
        });

        MiscArgs {
            device: registration.device(),
            parent,
            data: registration.data(),
        }
    }

    /// # Safety
    ///
    /// `file` and `inode` must be the file and inode for a file that is undergoing initialization.
    /// The file must be associated with a `MiscDeviceRegistration<T>`.
    unsafe extern "C" fn open(inode: *mut bindings::inode, raw_file: *mut bindings::file) -> c_int {
        // SAFETY: The pointers are valid and for a file being opened.
        let ret = unsafe { bindings::generic_file_open(inode, raw_file) };
        if ret != 0 {
            return ret;
        }

        // SAFETY: Called from a misc device file operation callback with the corresponding pointer
        // to a `struct file`.
        let registration = unsafe { Self::registration_from_file(raw_file) };

        // SAFETY:
        // * This underlying file is valid for (much longer than) the duration of `T::open`.
        // * There is no active fdget_pos region on the file on this thread.
        let file = unsafe { File::from_raw_file(raw_file) };

        let ptr = match T::open(file, Self::args_from_registration(registration)) {
            Ok(ptr) => ptr,
            Err(err) => return err.to_errno(),
        };

        // SAFETY:
        // * We only ever write `registration.private` from `open()`, which does not race with other
        //   file operation callbacks, i.e. there are no concurrent reads.
        // * `registration.private.get()` is properly aligned.
        unsafe { registration.private.get().write(ptr) };

        0
    }

    /// # Safety
    ///
    /// `file` and `inode` must be the file and inode for a file that is being released. The file
    /// must be associated with a `MiscDeviceRegistration<T>`.
    unsafe extern "C" fn release(_inode: *mut bindings::inode, file: *mut bindings::file) -> c_int {
        // SAFETY: Called from a misc device file operation callback with the corresponding pointer
        // to a `struct file`.
        let registration = unsafe { Self::registration_from_file(file) };

        // SAFETY:
        // * There won't be any subsequent reads or writes to `registration.private` once
        //   `release()` is called.
        // * `registration.private` has been initialized in `open()`.
        // * `registration.private.get()` is properly aligned.
        unsafe { core::ptr::drop_in_place(registration.private.get()) };

        0
    }

    /// # Safety
    ///
    /// `file` must be a valid file that is associated with a `MiscDeviceRegistration<T>`.
    unsafe extern "C" fn ioctl(file: *mut bindings::file, cmd: c_uint, arg: c_ulong) -> c_long {
        // SAFETY: Called from a misc device file operation callback with the corresponding pointer
        // to a `struct file`.
        let registration = unsafe { Self::registration_from_file(file) };

        // SAFETY:
        // * `registration.private` is initialized in `open()`, which is guaranteed to called
        //   before this callback.
        // * `registration.private.get()` is properly aligned.
        // * There are no concurrent writes.
        let private = unsafe { &*registration.private.get() };

        // SAFETY:
        // * The file is valid for the duration of this call.
        // * There is no active fdget_pos region on the file on this thread.
        let file = unsafe { File::from_raw_file(file) };

        let args = Self::args_from_registration(registration);

        match T::ioctl(args, private, file, cmd, arg) {
            Ok(ret) => ret as c_long,
            Err(err) => err.to_errno() as c_long,
        }
    }

    /// # Safety
    ///
    /// `file` must be a valid file that is associated with a `MiscDeviceRegistration<T>`.
    #[cfg(CONFIG_COMPAT)]
    unsafe extern "C" fn compat_ioctl(
        file: *mut bindings::file,
        cmd: c_uint,
        arg: c_ulong,
    ) -> c_long {
        // SAFETY: Called from a misc device file operation callback with the corresponding pointer
        // to a `struct file`.
        let registration = unsafe { Self::registration_from_file(file) };

        // SAFETY:
        // * `registration.private` is initialized in `open()`, which is guaranteed to called
        //   before this callback.
        // * `registration.private.get()` is properly aligned.
        // * There are no concurrent writes.
        let private = unsafe { &*registration.private.get() };

        // SAFETY:
        // * The file is valid for the duration of this call.
        // * There is no active fdget_pos region on the file on this thread.
        let file = unsafe { File::from_raw_file(file) };

        let args = Self::args_from_registration(registration);

        match T::compat_ioctl(args, private, file, cmd, arg) {
            Ok(ret) => ret as c_long,
            Err(err) => err.to_errno() as c_long,
        }
    }

    /// # Safety
    ///
    /// - `file` must be a valid file that is associated with a `MiscDeviceRegistration<T>`.
    /// - `seq_file` must be a valid `struct seq_file` that we can write to.
    unsafe extern "C" fn show_fdinfo(seq_file: *mut bindings::seq_file, file: *mut bindings::file) {
        // SAFETY: Called from a misc device file operation callback with the corresponding pointer
        // to a `struct file`.
        let registration = unsafe { Self::registration_from_file(file) };

        // SAFETY:
        // * `registration.private` is initialized in `open()`, which is guaranteed to called
        //   before this callback.
        // * `registration.private.get()` is properly aligned.
        // * There are no concurrent writes.
        let private = unsafe { &*registration.private.get() };

        // SAFETY:
        // * The file is valid for the duration of this call.
        // * There is no active fdget_pos region on the file on this thread.
        let file = unsafe { File::from_raw_file(file) };
        // SAFETY: The caller ensures that the pointer is valid and exclusive for the duration in
        // which this method is called.
        let m = unsafe { SeqFile::from_raw(seq_file) };

        let args = Self::args_from_registration(registration);

        T::show_fdinfo(args, private, m, file);
    }

    const VTABLE: bindings::file_operations = bindings::file_operations {
        open: Some(Self::open),
        release: Some(Self::release),
        unlocked_ioctl: if T::HAS_IOCTL {
            Some(Self::ioctl)
        } else {
            None
        },
        #[cfg(CONFIG_COMPAT)]
        compat_ioctl: if T::HAS_COMPAT_IOCTL {
            Some(Self::compat_ioctl)
        } else if T::HAS_IOCTL {
            Some(bindings::compat_ptr_ioctl)
        } else {
            None
        },
        show_fdinfo: if T::HAS_SHOW_FDINFO {
            Some(Self::show_fdinfo)
        } else {
            None
        },
        // SAFETY: All zeros is a valid value for `bindings::file_operations`.
        ..unsafe { MaybeUninit::zeroed().assume_init() }
    };

    const fn build() -> &'static bindings::file_operations {
        &Self::VTABLE
    }
}
