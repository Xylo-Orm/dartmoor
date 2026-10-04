use std::slice;

use screencapturekit::cm_sample_buffer::CMSampleBuffer;
use screencapturekit_sys::cm_sample_buffer_ref::CMSampleBufferGetSampleAttachmentsArray;

use super::super::mac_frame_state::FrameStatus;
use super::{
    apple_sys::*,
    pixel_buffer::{pixel_buffer_bounds, sample_buffer_to_pixel_buffer},
};
use crate::frame::{
    convert_bgra_to_rgb, get_cropped_data, remove_alpha_channel, BGRAFrame, BGRFrame, RGBFrame,
    YUVFrame,
};
use core_graphics_helmer_fork::display::{CFArrayGetCount, CFArrayGetValueAtIndex, CFArrayRef};
use core_video_sys::{
    kCVReturnSuccess, CVPixelBufferGetBaseAddress, CVPixelBufferGetBaseAddressOfPlane,
    CVPixelBufferGetBytesPerRow, CVPixelBufferGetBytesPerRowOfPlane, CVPixelBufferLockBaseAddress,
    CVPixelBufferRef, CVPixelBufferUnlockBaseAddress,
};

/// Pair every successful CoreVideo lock with an unlock, including early
/// returns for missing or invalid image data. CoreVideo requires a successful
/// CPU lock and a matching unlock with the same flags:
/// https://developer.apple.com/documentation/corevideo/cvpixelbufferlockbaseaddress(_:_:)
struct LockedPixelBuffer(CVPixelBufferRef);

impl LockedPixelBuffer {
    unsafe fn new(buffer: CVPixelBufferRef) -> Option<Self> {
        if buffer.is_null() || CVPixelBufferLockBaseAddress(buffer, 0) != kCVReturnSuccess {
            return None;
        }
        Some(Self(buffer))
    }
}

impl Drop for LockedPixelBuffer {
    fn drop(&mut self) {
        unsafe {
            CVPixelBufferUnlockBaseAddress(self.0, 0);
        }
    }
}

// Returns a frame's presentation timestamp in nanoseconds since an arbitrary start time.
// This is typically yielded from a monotonic clock started on system boot.
pub fn get_pts_in_nanoseconds(sample_buffer: &CMSampleBuffer) -> u64 {
    let pts = sample_buffer.sys_ref.get_presentation_timestamp();

    let seconds = unsafe { CMTimeGetSeconds(pts) };

    (seconds * 1_000_000_000.).trunc() as u64
}

/// Read only an explicit, well-typed status from the native attachments.
/// The 0.2.8 wrapper defaults missing metadata to Idle, which cannot establish
/// that a previously captured image is still current. Apple's status values:
/// https://developer.apple.com/documentation/screencapturekit/scframestatus
pub fn explicit_frame_status(sample_buffer: &CMSampleBuffer) -> FrameStatus {
    FrameStatus::from_raw(unsafe { raw_frame_status(sample_buffer) })
}

unsafe fn raw_frame_status(sample_buffer: &CMSampleBuffer) -> Option<i64> {
    let buffer_ref = &(*sample_buffer.sys_ref);
    unsafe {
        let attachments = CMSampleBufferGetSampleAttachmentsArray(buffer_ref, 0);
        if attachments.is_null() || CFArrayGetCount(attachments as CFArrayRef) == 0 {
            return None;
        }
        let attachment = CFArrayGetValueAtIndex(attachments as CFArrayRef, 0) as CFDictionaryRef;
        if attachment.is_null() || CFGetTypeID(attachment as CFTypeRef) != CFDictionaryGetTypeID() {
            return None;
        }
        let frame_status_ref = CFDictionaryGetValue(
            attachment as CFDictionaryRef,
            SCStreamFrameInfoStatus.0 as _,
        ) as CFTypeRef;
        if frame_status_ref.is_null() || CFGetTypeID(frame_status_ref) != CFNumberGetTypeID() {
            return None;
        }
        let mut frame_status: i64 = 0;
        let result = CFNumberGetValue(
            frame_status_ref as _,
            CFNumberType_kCFNumberSInt64Type,
            (&mut frame_status as *mut i64).cast(),
        );
        if result == 0 {
            return None;
        }
        Some(frame_status)
    }
}

pub unsafe fn create_yuv_frame(sample_buffer: CMSampleBuffer) -> Option<YUVFrame> {
    if explicit_frame_status(&sample_buffer) != FrameStatus::Complete {
        return None;
    }

    let display_time = get_pts_in_nanoseconds(&sample_buffer);
    let pixel_buffer = sample_buffer_to_pixel_buffer(&sample_buffer);

    let _locked = LockedPixelBuffer::new(pixel_buffer)?;

    let (width, height) = pixel_buffer_bounds(pixel_buffer);
    if width == 0 || height == 0 || width > i32::MAX as usize || height > i32::MAX as usize {
        return None;
    }

    let luminance_bytes_address = CVPixelBufferGetBaseAddressOfPlane(pixel_buffer, 0);
    let luminance_stride = CVPixelBufferGetBytesPerRowOfPlane(pixel_buffer, 0);
    if luminance_bytes_address.is_null()
        || luminance_stride < width
        || height.checked_mul(luminance_stride)? > isize::MAX as usize
    {
        return None;
    }
    let luminance_bytes = slice::from_raw_parts(
        luminance_bytes_address as *mut u8,
        height * luminance_stride,
    )
    .to_vec();

    let chrominance_bytes_address = CVPixelBufferGetBaseAddressOfPlane(pixel_buffer, 1);
    let chrominance_stride = CVPixelBufferGetBytesPerRowOfPlane(pixel_buffer, 1);
    if chrominance_bytes_address.is_null()
        || chrominance_stride < width
        || height.checked_mul(chrominance_stride)? > isize::MAX as usize
    {
        return None;
    }
    let chrominance_bytes = slice::from_raw_parts(
        chrominance_bytes_address as *mut u8,
        height * chrominance_stride / 2,
    )
    .to_vec();

    YUVFrame {
        display_time,
        width: width as i32,
        height: height as i32,
        luminance_bytes,
        luminance_stride: luminance_stride as i32,
        chrominance_bytes,
        chrominance_stride: chrominance_stride as i32,
    }
    .into()
}

pub unsafe fn create_bgr_frame(sample_buffer: CMSampleBuffer) -> Option<BGRFrame> {
    let pixel_buffer = sample_buffer_to_pixel_buffer(&sample_buffer);
    let display_time = get_pts_in_nanoseconds(&sample_buffer);

    let _locked = LockedPixelBuffer::new(pixel_buffer)?;

    let (width, height) = pixel_buffer_bounds(pixel_buffer);
    if width == 0 || height == 0 || width > i32::MAX as usize || height > i32::MAX as usize {
        return None;
    }

    let base_address = CVPixelBufferGetBaseAddress(pixel_buffer);
    let bytes_per_row = CVPixelBufferGetBytesPerRow(pixel_buffer);
    if base_address.is_null()
        || bytes_per_row < width.checked_mul(4)?
        || bytes_per_row.checked_mul(height)? > isize::MAX as usize
    {
        return None;
    }

    let data = slice::from_raw_parts(base_address as *mut u8, bytes_per_row * height).to_vec();

    let cropped_data = get_cropped_data(
        data,
        (bytes_per_row / 4) as i32,
        height as i32,
        width as i32,
    );

    Some(BGRFrame {
        display_time,
        width: width as i32, // width does not give accurate results - https://stackoverflow.com/questions/19587185/cvpixelbuffergetbytesperrow-for-cvimagebufferref-returns-unexpected-wrong-valu
        height: height as i32,
        data: remove_alpha_channel(cropped_data),
    })
}

pub unsafe fn create_bgra_frame(sample_buffer: CMSampleBuffer) -> Option<BGRAFrame> {
    let pixel_buffer = sample_buffer_to_pixel_buffer(&sample_buffer);
    let display_time = get_pts_in_nanoseconds(&sample_buffer);

    let _locked = LockedPixelBuffer::new(pixel_buffer)?;

    let (width, height) = pixel_buffer_bounds(pixel_buffer);
    if width == 0 || height == 0 || width > i32::MAX as usize || height > i32::MAX as usize {
        return None;
    }

    let base_address = CVPixelBufferGetBaseAddress(pixel_buffer);
    let bytes_per_row = CVPixelBufferGetBytesPerRow(pixel_buffer);
    if base_address.is_null()
        || bytes_per_row < width.checked_mul(4)?
        || bytes_per_row.checked_mul(height)? > isize::MAX as usize
    {
        return None;
    }

    let mut data: Vec<u8> = vec![];

    for i in 0..height {
        let start = (base_address as *mut u8).wrapping_add(i * bytes_per_row);
        data.extend_from_slice(slice::from_raw_parts(start, 4 * width));
    }

    Some(BGRAFrame {
        display_time,
        width: width as i32, // width does not give accurate results - https://stackoverflow.com/questions/19587185/cvpixelbuffergetbytesperrow-for-cvimagebufferref-returns-unexpected-wrong-valu
        height: height as i32,
        data,
    })
}

pub unsafe fn create_rgb_frame(sample_buffer: CMSampleBuffer) -> Option<RGBFrame> {
    let pixel_buffer = sample_buffer_to_pixel_buffer(&sample_buffer);
    let display_time = get_pts_in_nanoseconds(&sample_buffer);

    let _locked = LockedPixelBuffer::new(pixel_buffer)?;

    let (width, height) = pixel_buffer_bounds(pixel_buffer);
    if width == 0 || height == 0 || width > i32::MAX as usize || height > i32::MAX as usize {
        return None;
    }

    let base_address = CVPixelBufferGetBaseAddress(pixel_buffer);
    let bytes_per_row = CVPixelBufferGetBytesPerRow(pixel_buffer);
    if base_address.is_null()
        || bytes_per_row < width.checked_mul(4)?
        || bytes_per_row.checked_mul(height)? > isize::MAX as usize
    {
        return None;
    }

    let data = slice::from_raw_parts(base_address as *mut u8, bytes_per_row * height).to_vec();

    let cropped_data = get_cropped_data(
        data,
        (bytes_per_row / 4) as i32,
        height as i32,
        width as i32,
    );

    Some(RGBFrame {
        display_time,
        width: width as i32, // width does not give accurate results - https://stackoverflow.com/questions/19587185/cvpixelbuffergetbytesperrow-for-cvimagebufferref-returns-unexpected-wrong-valu
        height: height as i32,
        data: convert_bgra_to_rgb(cropped_data),
    })
}
