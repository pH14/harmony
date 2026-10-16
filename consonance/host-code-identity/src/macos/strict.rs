// SPDX-License-Identifier: AGPL-3.0-or-later

use std::ffi::c_void;
use std::fs::File;
use std::io;
use std::mem::size_of;
use std::os::fd::AsRawFd;
use std::ptr;

use sha2::{Digest, Sha256};

use super::{Generation, copy_mapped, dyld_metadata, generation, generation_is_stable, refused};

type CfRef = *const c_void;

const CS_VALID: u32 = 0x0000_0001;
const CS_HARD: u32 = 0x0000_0100;
const CS_KILL: u32 = 0x0000_0200;
const CS_RUNTIME: u32 = 0x0001_0000;
const CS_DEBUGGED: u32 = 0x1000_0000;
const CS_OPS_STATUS: u32 = 0;
const CS_OPS_CDHASH: u32 = 5;
const K_SEC_CS_STRICT_VALIDATE: u32 = 1 << 4;
const K_SEC_CS_INTERNAL_AND_SIGNING_INFORMATION: u32 = 3;
const K_CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;
const MH_EXECUTE: u32 = 2;
const MH_DYLIB: u32 = 6;
const MH_DYLINKER: u32 = 7;
const MH_DYLIB_IN_CACHE: u32 = 0x8000_0000;
const OS_ONLY_LIBRARY_CONSTRAINT: [u8; 84] = [
    0xfa, 0xde, 0x81, 0x81, 0x00, 0x00, 0x00, 0x54, 0x70, 0x4a, 0x02, 0x01, 0x01, 0xb0, 0x45, 0x30,
    0x09, 0x0c, 0x04, 0x63, 0x63, 0x61, 0x74, 0x02, 0x01, 0x00, 0x30, 0x09, 0x0c, 0x04, 0x63, 0x6f,
    0x6d, 0x70, 0x02, 0x01, 0x01, 0x30, 0x22, 0x0c, 0x04, 0x72, 0x65, 0x71, 0x73, 0xb0, 0x1a, 0x30,
    0x18, 0x0c, 0x13, 0x76, 0x61, 0x6c, 0x69, 0x64, 0x61, 0x74, 0x69, 0x6f, 0x6e, 0x2d, 0x63, 0x61,
    0x74, 0x65, 0x67, 0x6f, 0x72, 0x79, 0x02, 0x01, 0x01, 0x30, 0x09, 0x0c, 0x04, 0x76, 0x65, 0x72,
    0x73, 0x02, 0x01, 0x01,
];

#[derive(Clone, Copy, Debug)]
pub struct StrictMacosExecutableGuard {
    status: u32,
    cdhash: [u8; 20],
    cache_uuid: [u8; 16],
    epoch: u64,
    images: u32,
}

struct OwnedCf(CfRef);

impl OwnedCf {
    fn new(value: CfRef, message: &'static str) -> io::Result<Self> {
        if value.is_null() {
            Err(refused(message))
        } else {
            Ok(Self(value))
        }
    }
}

impl Drop for OwnedCf {
    fn drop(&mut self) {
        // SAFETY: OwnedCf is created only from a non-null retained CoreFoundation result.
        unsafe { CFRelease(self.0) };
    }
}

#[link(name = "Security", kind = "framework")]
unsafe extern "C" {
    static kSecCodeInfoUnique: CfRef;
    static kSecCodeInfoEntitlementsDict: CfRef;
    fn SecStaticCodeCreateWithPath(url: CfRef, flags: u32, code: *mut CfRef) -> i32;
    fn SecStaticCodeCheckValidity(code: CfRef, flags: u32, requirement: CfRef) -> i32;
    fn SecCodeCopySigningInformation(code: CfRef, flags: u32, information: *mut CfRef) -> i32;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRelease(value: CfRef);
    fn CFURLCreateFromFileSystemRepresentation(
        allocator: CfRef,
        bytes: *const u8,
        length: isize,
        is_directory: u8,
    ) -> CfRef;
    fn CFStringCreateWithCString(allocator: CfRef, bytes: *const i8, encoding: u32) -> CfRef;
    fn CFDictionaryGetValue(dictionary: CfRef, key: CfRef) -> CfRef;
    fn CFDictionaryGetCount(dictionary: CfRef) -> isize;
    fn CFGetTypeID(value: CfRef) -> usize;
    fn CFDataGetTypeID() -> usize;
    fn CFDictionaryGetTypeID() -> usize;
    fn CFBooleanGetTypeID() -> usize;
    fn CFBooleanGetValue(boolean: CfRef) -> u8;
    fn CFDataGetLength(data: CfRef) -> isize;
    fn CFDataGetBytePtr(data: CfRef) -> *const u8;
}

unsafe extern "C" {
    fn csops(pid: libc::pid_t, operation: u32, buffer: *mut c_void, length: usize) -> i32;
}

fn kernel_code_state() -> io::Result<(u32, [u8; 20])> {
    let mut status = 0_u32;
    let mut cdhash = [0_u8; 20];
    // SAFETY: both output buffers are writable for the exact lengths passed, and getpid names
    // this process. The kernel returns an error if either code-signing query is unavailable.
    let result = unsafe {
        let pid = libc::getpid();
        (
            csops(
                pid,
                CS_OPS_STATUS,
                (&raw mut status).cast(),
                size_of::<u32>(),
            ),
            csops(pid, CS_OPS_CDHASH, cdhash.as_mut_ptr().cast(), cdhash.len()),
        )
    };
    if result != (0, 0) || !acceptable_kernel_status(status) || cdhash == [0; 20] {
        return Err(refused("macOS kernel code-signing state is not strict"));
    }
    Ok((status, cdhash))
}

fn acceptable_kernel_status(status: u32) -> bool {
    status & (CS_VALID | CS_HARD | CS_KILL | CS_RUNTIME)
        == CS_VALID | CS_HARD | CS_KILL | CS_RUNTIME
        && status & CS_DEBUGGED == 0
}

fn cf_string(value: &'static [u8]) -> io::Result<OwnedCf> {
    // SAFETY: each caller supplies a statically allocated NUL-terminated UTF-8 byte string.
    OwnedCf::new(
        unsafe {
            CFStringCreateWithCString(
                ptr::null(),
                value.as_ptr().cast(),
                K_CF_STRING_ENCODING_UTF8,
            )
        },
        "macOS code-signing key is unavailable",
    )
}

fn dictionary_value(dictionary: CfRef, key: CfRef) -> io::Result<CfRef> {
    if dictionary.is_null() {
        return Err(refused("macOS code-signing information is absent"));
    }
    // SAFETY: dictionary is retained by the caller and has the checked CFDictionary type.
    let value = unsafe { CFDictionaryGetValue(dictionary, key) };
    if value.is_null() {
        Err(refused("macOS required code-signing field is absent"))
    } else {
        Ok(value)
    }
}

fn cf_data(value: CfRef, length: usize) -> io::Result<Vec<u8>> {
    // SAFETY: value is an object retained by its parent information dictionary.
    if unsafe { CFGetTypeID(value) } != unsafe { CFDataGetTypeID() } {
        return Err(refused("macOS code-signing field has unexpected type"));
    }
    // SAFETY: CFDataGetLength and CFDataGetBytePtr observe the same retained immutable CFData.
    let actual = unsafe { CFDataGetLength(value) };
    if usize::try_from(actual).ok() != Some(length) {
        return Err(refused("macOS code-signing field has unexpected length"));
    }
    // SAFETY: a positive exact length above ensures the returned pointer covers length bytes
    // while the parent dictionary remains retained, and the bytes are copied immediately.
    let bytes = unsafe { CFDataGetBytePtr(value) };
    if bytes.is_null() {
        return Err(refused("macOS code-signing field has no bytes"));
    }
    Ok(unsafe { std::slice::from_raw_parts(bytes, length) }.to_vec())
}

fn strict_static_cdhash(file: &File) -> io::Result<[u8; 20]> {
    let path = format!("/dev/fd/{}", file.as_raw_fd());
    let path_bytes = path.as_bytes();
    // SAFETY: path_bytes is alive for the call and is a valid nonempty file URL path; the
    // opened file remains held through every Security.framework validation below.
    let url = OwnedCf::new(
        unsafe {
            CFURLCreateFromFileSystemRepresentation(
                ptr::null(),
                path_bytes.as_ptr(),
                isize::try_from(path_bytes.len())
                    .map_err(|_| refused("macOS executable fd path is too long"))?,
                0,
            )
        },
        "macOS executable fd URL is unavailable",
    )?;
    let mut raw_code = ptr::null();
    // SAFETY: url is a retained CFURL and raw_code is a writable output pointer.
    let created = unsafe { SecStaticCodeCreateWithPath(url.0, 0, &raw mut raw_code) };
    if created != 0 {
        return Err(refused("macOS executable signature cannot be opened"));
    }
    let code = OwnedCf::new(raw_code, "macOS executable signature is absent")?;
    // SAFETY: code is a retained SecStaticCodeRef and null means no additional requirement.
    if unsafe { SecStaticCodeCheckValidity(code.0, K_SEC_CS_STRICT_VALIDATE, ptr::null()) } != 0 {
        return Err(refused(
            "macOS executable signature failed strict validation",
        ));
    }
    let mut raw_info = ptr::null();
    // SAFETY: code is valid and raw_info is a writable output pointer for a retained dictionary.
    let copied = unsafe {
        SecCodeCopySigningInformation(
            code.0,
            K_SEC_CS_INTERNAL_AND_SIGNING_INFORMATION,
            &raw mut raw_info,
        )
    };
    if copied != 0 {
        return Err(refused(
            "macOS executable signature information is unavailable",
        ));
    }
    let info = OwnedCf::new(raw_info, "macOS executable signature information is absent")?;
    // SAFETY: info is retained and the Security API promises a CFDictionary result.
    if unsafe { CFGetTypeID(info.0) } != unsafe { CFDictionaryGetTypeID() } {
        return Err(refused(
            "macOS executable signature information has unexpected type",
        ));
    }
    let library_key = cf_string(b"LibraryConstraints\0")?;
    let constraint = cf_data(
        dictionary_value(info.0, library_key.0)?,
        OS_ONLY_LIBRARY_CONSTRAINT.len(),
    )?;
    if constraint != OS_ONLY_LIBRARY_CONSTRAINT {
        return Err(refused(
            "macOS executable lacks the exact OS-only library constraint",
        ));
    }
    // SAFETY: this exported Security.framework constant is a valid immutable CFStringRef.
    let entitlements = dictionary_value(info.0, unsafe { kSecCodeInfoEntitlementsDict })?;
    // SAFETY: entitlements is retained by info and type checked before dictionary operations.
    if unsafe { CFGetTypeID(entitlements) } != unsafe { CFDictionaryGetTypeID() }
        || unsafe { CFDictionaryGetCount(entitlements) } != 1
    {
        return Err(refused("macOS executable entitlements are not strict"));
    }
    let hypervisor_key = cf_string(b"com.apple.security.hypervisor\0")?;
    let hypervisor = dictionary_value(entitlements, hypervisor_key.0)?;
    // SAFETY: hypervisor is retained by entitlements; its CFBoolean type is checked first.
    if unsafe { CFGetTypeID(hypervisor) } != unsafe { CFBooleanGetTypeID() }
        || unsafe { CFBooleanGetValue(hypervisor) } == 0
    {
        return Err(refused("macOS executable lacks the Hypervisor entitlement"));
    }
    // SAFETY: this exported Security.framework constant is a valid immutable CFStringRef.
    let unique = cf_data(dictionary_value(info.0, unsafe { kSecCodeInfoUnique })?, 20)?;
    Ok(unique.try_into().unwrap())
}

fn image_header(header: usize) -> io::Result<(u32, u32)> {
    let mut bytes = [0_u8; 32];
    copy_mapped(header, &mut bytes)?;
    if u32::from_le_bytes(bytes[0..4].try_into().unwrap()) != 0xfeedfacf {
        return Err(refused("macOS loaded image is not a 64-bit Mach-O"));
    }
    Ok((
        u32::from_le_bytes(bytes[12..16].try_into().unwrap()),
        u32::from_le_bytes(bytes[24..28].try_into().unwrap()),
    ))
}

fn image_is_allowed(
    index: u32,
    header: usize,
    cache_start: usize,
    cache_end: usize,
) -> io::Result<()> {
    let (kind, flags) = image_header(header)?;
    if index == 0 {
        if kind != MH_EXECUTE || flags & MH_DYLIB_IN_CACHE != 0 {
            return Err(refused("macOS main image has unexpected loaded identity"));
        }
    } else if kind != MH_DYLIB
        || flags & MH_DYLIB_IN_CACHE == 0
        || header < cache_start
        || header >= cache_end
    {
        return Err(refused("macOS non-system library is loaded"));
    }
    Ok(())
}

fn strict_shared_cache_uuid() -> io::Result<([u8; 16], Generation)> {
    let start = generation()?;
    let dyld = dyld_metadata()?;
    if dyld.cache_uuid == [0; 16] {
        return Err(refused("macOS dyld shared cache identity is absent"));
    }
    // SAFETY: dlsym queries a process-global symbol by a static NUL-terminated name.
    let symbol =
        unsafe { libc::dlsym(libc::RTLD_DEFAULT, c"_dyld_get_shared_cache_range".as_ptr()) };
    if symbol.is_null() {
        return Err(refused("macOS dyld shared cache range is unavailable"));
    }
    // SAFETY: the symbol is the dyld function with the exact function signature used here.
    let range: unsafe extern "C" fn(*mut usize) -> *const c_void =
        unsafe { std::mem::transmute(symbol) };
    let mut length = 0_usize;
    // SAFETY: length is writable and dyld returns this process's mapped cache range.
    let cache_start = unsafe { range(&raw mut length) } as usize;
    let cache_end = cache_start
        .checked_add(length)
        .filter(|end| cache_start != 0 && length != 0 && *end > cache_start)
        .ok_or_else(|| refused("macOS dyld shared cache range is invalid"))?;
    let (dyld_kind, dyld_flags) = image_header(dyld.header)?;
    if dyld_kind != MH_DYLINKER
        || dyld_flags & MH_DYLIB_IN_CACHE == 0
        || dyld.header < cache_start
        || dyld.header >= cache_end
    {
        return Err(refused("macOS dyld is not loaded from the shared cache"));
    }
    for index in 0..start.images {
        // SAFETY: index is below the sampled image count, each address is read through the
        // kernel, and the generation is checked again after the complete census.
        #[allow(deprecated)]
        let header = unsafe { libc::_dyld_get_image_header(index) } as usize;
        if header == 0 {
            return Err(refused("macOS loaded-image record is null"));
        }
        image_is_allowed(index, header, cache_start, cache_end)?;
    }
    let finish_dyld = dyld_metadata()?;
    let finish = generation()?;
    if !generation_is_stable(start, finish) || dyld != finish_dyld {
        return Err(refused(
            "macOS loaded images changed during strict admission",
        ));
    }
    Ok((dyld.cache_uuid, start))
}

impl StrictMacosExecutableGuard {
    pub fn capture() -> io::Result<(String, Self)> {
        let (start_status, loaded_cdhash) = kernel_code_state()?;
        let file = File::open(std::env::current_exe()?)?;
        let static_cdhash = strict_static_cdhash(&file)?;
        if loaded_cdhash != static_cdhash {
            return Err(refused(
                "macOS validated file differs from the loaded executable",
            ));
        }
        let (cache_uuid, generation) = strict_shared_cache_uuid()?;
        let (finish_status, finish_cdhash) = kernel_code_state()?;
        if start_status != finish_status || loaded_cdhash != finish_cdhash {
            return Err(refused(
                "macOS loaded executable identity changed during admission",
            ));
        }
        let mut digest = Sha256::new();
        digest.update(b"harmony-macos-strict-host-v1\0");
        digest.update(loaded_cdhash);
        digest.update(cache_uuid);
        Ok((
            format!("macos-strict-host-sha256-v1:{:x}", digest.finalize()),
            Self {
                status: start_status,
                cdhash: loaded_cdhash,
                cache_uuid,
                epoch: generation.epoch,
                images: generation.images,
            },
        ))
    }

    pub fn check(self) -> io::Result<()> {
        let (start_status, start_cdhash) = kernel_code_state()?;
        let (cache_uuid, generation) = strict_shared_cache_uuid()?;
        let (finish_status, finish_cdhash) = kernel_code_state()?;
        if start_status != finish_status
            || start_cdhash != finish_cdhash
            || start_status != self.status
            || start_cdhash != self.cdhash
            || cache_uuid != self.cache_uuid
            || generation.epoch != self.epoch
            || generation.images != self.images
        {
            return Err(refused("macOS loaded executable changed after admission"));
        }
        Ok(())
    }
}

pub fn strict_macos_executable_identity() -> io::Result<String> {
    Ok(StrictMacosExecutableGuard::capture()?.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;

    #[link(name = "Hypervisor", kind = "framework")]
    unsafe extern "C" {
        fn hv_vm_create(options: *const c_void) -> i32;
        fn hv_vm_destroy() -> i32;
    }

    #[used]
    static mut STRICT_DATA_PROBE: [u8; 34] = *b"HARMONY_STRICT_DATA_PROBE_A_000000";

    const CHILD_ENV: &str = "HARMONY_STRICT_MACOS_TEST_CHILD";
    const CHILD_TEST: &str = "macos::strict::tests::signed_executable_admission";

    fn sign(path: &std::path::Path, runtime: bool, constraint: bool, extra: bool) {
        let directory = path.parent().unwrap();
        let entitlements = directory.join(if extra {
            "extra.plist"
        } else {
            "hypervisor.plist"
        });
        fs::write(
            &entitlements,
            if extra {
                "<plist version=\"1.0\"><dict><key>com.apple.security.hypervisor</key><true/><key>com.apple.security.cs.disable-library-validation</key><true/></dict></plist>"
            } else {
                "<plist version=\"1.0\"><dict><key>com.apple.security.hypervisor</key><true/></dict></plist>"
            },
        )
        .unwrap();
        let library = directory.join("os-only.plist");
        fs::write(
            &library,
            "<plist version=\"1.0\"><dict><key>validation-category</key><integer>1</integer></dict></plist>",
        )
        .unwrap();
        let mut command = Command::new("codesign");
        command.args([
            "--force",
            "--sign",
            "-",
            "--identifier",
            "org.harmony.strict-host-test",
        ]);
        if runtime {
            command.args(["--options", "runtime,hard,kill"]);
        }
        command.arg("--entitlements").arg(&entitlements);
        if constraint {
            command.arg("--library-constraint").arg(&library);
        }
        let output = command.arg(path).output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn signed_child(path: &std::path::Path) -> std::process::Output {
        Command::new(path)
            .args(["--exact", CHILD_TEST, "--nocapture"])
            .env(CHILD_ENV, "1")
            .output()
            .unwrap()
    }

    fn probe_output(output: &std::process::Output) -> String {
        assert!(
            output.status.success(),
            "{}\n{}\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn signed_executable_admission() {
        if std::env::var_os(CHILD_ENV).is_some() {
            // SAFETY: STRICT_DATA_PROBE is initialized for the process and this test only reads
            // one byte through a raw pointer; no code mutates the static after launch.
            let data = unsafe {
                std::ptr::read_volatile((&raw const STRICT_DATA_PROBE).cast::<u8>().add(26))
            };
            let handshake = std::env::var_os("HARMONY_STRICT_MACOS_REPLACE_HANDSHAKE");
            let runtime_guard = handshake
                .as_ref()
                .map(|_| StrictMacosExecutableGuard::capture().unwrap().1);
            if let Some(handshake) = handshake {
                let directory = std::path::Path::new(&handshake);
                fs::write(directory.join("ready"), b"").unwrap();
                for _ in 0..500 {
                    if directory.join("go").exists() {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                assert!(directory.join("go").exists());
            }
            if let Some(runtime_guard) = runtime_guard {
                println!("loaded_guard={}", runtime_guard.check().is_ok());
            }
            match strict_macos_executable_identity() {
                Ok(identity) => {
                    // SAFETY: null selects default VM options; a successful create owns one
                    // process-local VM that this test destroys before returning.
                    assert_eq!(unsafe { hv_vm_create(ptr::null()) }, 0);
                    // SAFETY: the preceding successful create established the VM being destroyed.
                    assert_eq!(unsafe { hv_vm_destroy() }, 0);
                    println!("strict_identity={identity} data={data}");
                }
                Err(error) => println!("strict_rejected={error} data={data}"),
            }
            return;
        }
        let directory = tempfile::tempdir().unwrap();
        let plain = directory.path().join("plain");
        let strict = directory.path().join("strict");
        let no_runtime = directory.path().join("no-runtime");
        let extra = directory.path().join("extra");
        fs::copy(std::env::current_exe().unwrap(), &plain).unwrap();
        fs::copy(&plain, &strict).unwrap();
        fs::copy(&plain, &no_runtime).unwrap();
        fs::copy(&plain, &extra).unwrap();
        sign(&plain, true, false, false);
        sign(&strict, true, true, false);
        sign(&no_runtime, false, true, false);
        sign(&extra, true, true, true);
        let strict_output = probe_output(&signed_child(&strict));
        assert!(
            strict_output.contains("strict_identity="),
            "{strict_output}"
        );
        for path in [&plain, &no_runtime, &extra] {
            let output = probe_output(&signed_child(path));
            assert!(output.contains("strict_rejected="), "{output}");
        }
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn data_initializer_and_replaced_path_change_admission() {
        let directory = tempfile::tempdir().unwrap();
        let a = directory.path().join("a");
        let b = directory.path().join("b");
        let initial = fs::read(std::env::current_exe().unwrap()).unwrap();
        // SAFETY: no code mutates this initialized test static after launch, and the raw copy
        // reads its complete fixed-size byte array without creating a shared reference.
        let marker = unsafe { std::ptr::read(&raw const STRICT_DATA_PROBE) };
        let positions = initial
            .windows(marker.len())
            .enumerate()
            .filter_map(|(index, window)| (window == marker).then_some(index))
            .collect::<Vec<_>>();
        assert_eq!(positions.len(), 1);
        let mut altered = initial.clone();
        altered[positions[0] + 26] = b'B';
        assert_eq!(
            initial
                .iter()
                .zip(&altered)
                .filter(|(left, right)| left != right)
                .count(),
            1
        );
        fs::write(&a, initial).unwrap();
        fs::write(&b, altered).unwrap();
        fs::set_permissions(&a, fs::Permissions::from_mode(0o755)).unwrap();
        fs::set_permissions(&b, fs::Permissions::from_mode(0o755)).unwrap();
        sign(&a, true, true, false);
        sign(&b, true, true, false);
        let output_a = probe_output(&signed_child(&a));
        let output_b = probe_output(&signed_child(&b));
        assert!(output_a.contains(" data=65"), "{output_a}");
        assert!(output_b.contains(" data=66"), "{output_b}");
        let identity_a = output_a
            .split("strict_identity=")
            .nth(1)
            .unwrap()
            .split(' ')
            .next()
            .unwrap();
        let identity_b = output_b
            .split("strict_identity=")
            .nth(1)
            .unwrap()
            .split(' ')
            .next()
            .unwrap();
        assert_ne!(identity_a, identity_b);

        let active = directory.path().join("active");
        let replacement = directory.path().join("replacement");
        fs::copy(&a, &active).unwrap();
        fs::copy(&b, &replacement).unwrap();
        let handshake = directory.path().join("handshake");
        fs::create_dir(&handshake).unwrap();
        let child = Command::new(&active)
            .args(["--exact", CHILD_TEST, "--nocapture"])
            .env(CHILD_ENV, "1")
            .env("HARMONY_STRICT_MACOS_REPLACE_HANDSHAKE", &handshake)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        for _ in 0..500 {
            if handshake.join("ready").exists() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(handshake.join("ready").exists());
        fs::rename(&replacement, &active).unwrap();
        fs::write(handshake.join("go"), b"").unwrap();
        let status = child.wait_with_output().unwrap();
        let replaced = probe_output(&status);
        assert!(replaced.contains("loaded_guard=true"), "{replaced}");
        assert!(replaced.contains("strict_rejected="), "{replaced}");
    }

    #[test]
    fn kernel_status_requires_hardened_runtime() {
        assert!(acceptable_kernel_status(0x2201_1311));
        for missing in [CS_VALID, CS_HARD, CS_KILL, CS_RUNTIME] {
            assert!(!acceptable_kernel_status(0x2201_1311 & !missing));
        }
        assert!(!acceptable_kernel_status(0x2201_1311 | CS_DEBUGGED));
    }
}
