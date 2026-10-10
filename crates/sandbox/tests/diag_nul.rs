#![cfg(windows)]
#![allow(unsafe_code)]
use bravebot_sandbox::base::{Prelude, base};
use bravebot_sandbox::policy::SandboxPolicy;
use bravebot_sandbox::windows::AppContainerSandbox;
use bravebot_sandbox::{Environment, Sandbox, Stream, Streams};
use std::ffi::OsString;
use std::io::Read;
use std::path::PathBuf;

fn scratch(name: &str) -> PathBuf {
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.pop();
    path.pop();
    path.push("target");
    path.push("test-scratch");
    path.push(format!("diag-{name}"));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).unwrap();
    path
}

fn run(sb: &AppContainerSandbox, program: &str, args: &[&str], policy: &SandboxPolicy) -> String {
    let args: Vec<OsString> = args.iter().map(OsString::from).collect();
    let streams = Streams {
        stdin: Stream::Null,
        stdout: Stream::Piped,
        stderr: Stream::Piped,
    };
    let mut child = match sb.spawn(
        program.as_ref(),
        &args,
        policy,
        streams,
        Environment::Inherited,
    ) {
        Ok(c) => c,
        Err(e) => return format!("REFUSED: {e:?}"),
    };
    let mut out = Vec::new();
    child.take_stdout().unwrap().read_to_end(&mut out).unwrap();
    let mut err = Vec::new();
    child.take_stderr().unwrap().read_to_end(&mut err).unwrap();
    let status = child.wait().unwrap();
    format!(
        "code={:?} out={:?} err={:?}",
        status.code(),
        String::from_utf8_lossy(&out).trim(),
        String::from_utf8_lossy(&err).trim()
    )
}

fn host(program: &str, args: &[&str]) {
    let out = std::process::Command::new(program).args(args).output();
    match out {
        Ok(o) => println!(
            "HOST {program} {args:?}\n  code={:?}\n  out={}\n  err={}",
            o.status.code(),
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        ),
        Err(e) => println!("HOST {program} {args:?} FAILED {e}"),
    }
}

#[test]
fn nul_diagnostic() {
    host("cmd.exe", &["/c", "ver"]);
    host(
        "powershell.exe",
        &[
            "-NoProfile",
            "-Command",
            "(Get-CimInstance Win32_OperatingSystem).Caption; whoami /all",
        ],
    );
    host(
        "powershell.exe",
        &[
            "-NoProfile",
            "-Command",
            "foreach ($p in '\\\\.\\NUL','\\\\?\\GLOBALROOT\\Device\\Null','NUL') { try { Write-Output \"$p => $((Get-Acl -LiteralPath $p).Sddl)\" } catch { Write-Output \"$p => ERR $_\" } }",
        ],
    );
    host("git.exe", &["--version"]);
    host("where.exe", &["git"]);
    let sb = AppContainerSandbox::new().unwrap();
    let t = scratch("tmp");
    let policy = base(Prelude::Windows, &t, None, None).starting_in(&t);
    for line in [
        "type nul",
        "echo x>nul",
        "echo x>\\\\.\\nul",
        "dir nul",
        "more < nul",
        "echo x 1>&2",
        "set",
    ] {
        println!(
            "CONFINED cmd /c {line:?}: {}",
            run(&sb, "cmd.exe", &["/c", line], &policy)
        );
    }
    println!("CONFINED git: {}", run(&sb, "git", &["--version"], &policy));
    for variant in [r"\\.\NUL", r"\\.\nul", r"\\?\GLOBALROOT\Device\Null"] {
        for write in [false, true] {
            let p = if write {
                policy.clone().allow_write(variant)
            } else {
                policy.clone().allow_read(variant)
            };
            println!(
                "CONFINED+grant {variant} write={write} cmd type nul: {}",
                run(&sb, "cmd.exe", &["/c", "type nul"], &p)
            );
        }
    }
}

use std::ptr::{null, null_mut};
use windows_sys::Win32::Foundation::{ERROR_SUCCESS, LocalFree};
use windows_sys::Win32::Security::Authorization::{
    ConvertSecurityDescriptorToStringSecurityDescriptorW, ConvertStringSidToSidW,
    EXPLICIT_ACCESS_W, GRANT_ACCESS, GetNamedSecurityInfoW, NO_MULTIPLE_TRUSTEE, SDDL_REVISION_1,
    SE_FILE_OBJECT, SetEntriesInAclW, SetNamedSecurityInfoW, TRUSTEE_IS_GROUP, TRUSTEE_IS_SID,
    TRUSTEE_W,
};
use windows_sys::Win32::Security::{
    ACL, DACL_SECURITY_INFORMATION, GROUP_SECURITY_INFORMATION, OWNER_SECURITY_INFORMATION,
    PSECURITY_DESCRIPTOR,
};

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn sddl_of(path: &str) -> String {
    let w = wide(path);
    let mut dacl: *mut ACL = null_mut();
    let mut sd: PSECURITY_DESCRIPTOR = null_mut();
    let r = unsafe {
        GetNamedSecurityInfoW(
            w.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | OWNER_SECURITY_INFORMATION | GROUP_SECURITY_INFORMATION,
            null_mut(),
            null_mut(),
            &mut dacl,
            null_mut(),
            &mut sd,
        )
    };
    if r != ERROR_SUCCESS {
        return format!("GetNamedSecurityInfoW error {r}");
    }
    let mut text: *mut u16 = null_mut();
    let ok = unsafe {
        ConvertSecurityDescriptorToStringSecurityDescriptorW(
            sd,
            SDDL_REVISION_1,
            DACL_SECURITY_INFORMATION | OWNER_SECURITY_INFORMATION | GROUP_SECURITY_INFORMATION,
            &mut text,
            null_mut(),
        )
    };
    if ok == 0 {
        return "convert failed".into();
    }
    let mut n = 0;
    while unsafe { *text.add(n) } != 0 {
        n += 1;
    }
    let s = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(text, n) });
    unsafe { LocalFree(text.cast()) };
    s
}

fn add_ace(path: &str, sid: &str, mask: u32) -> String {
    let w = wide(path);
    let mut dacl: *mut ACL = null_mut();
    let mut sd: PSECURITY_DESCRIPTOR = null_mut();
    let r = unsafe {
        GetNamedSecurityInfoW(
            w.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            null_mut(),
            null_mut(),
            &mut dacl,
            null_mut(),
            &mut sd,
        )
    };
    if r != ERROR_SUCCESS {
        return format!("get error {r}");
    }
    let mut psid = null_mut();
    let ws = wide(sid);
    if unsafe { ConvertStringSidToSidW(ws.as_ptr(), &mut psid) } == 0 {
        return "sid parse failed".into();
    }
    let entry = EXPLICIT_ACCESS_W {
        grfAccessPermissions: mask,
        grfAccessMode: GRANT_ACCESS,
        grfInheritance: 0,
        Trustee: TRUSTEE_W {
            pMultipleTrustee: null_mut(),
            MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
            TrusteeForm: TRUSTEE_IS_SID,
            TrusteeType: TRUSTEE_IS_GROUP,
            ptstrName: psid.cast(),
        },
    };
    let mut merged: *mut ACL = null_mut();
    let b = unsafe { SetEntriesInAclW(1, &entry, dacl, &mut merged) };
    if b != ERROR_SUCCESS {
        return format!("SetEntriesInAcl error {b}");
    }
    let s = unsafe {
        SetNamedSecurityInfoW(
            w.as_ptr() as *mut u16,
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            null_mut(),
            null_mut(),
            merged,
            null(),
        )
    };
    format!("SetNamedSecurityInfoW => {s}")
}

#[test]
fn nul_acl_experiment() {
    for path in [
        r"\\.\NUL",
        r"\\.\GLOBALROOT\Device\Null",
        r"\\?\GLOBALROOT\Device\Null",
        "NUL",
    ] {
        println!("SDDL {path}: {}", sddl_of(path));
    }
    let sb = AppContainerSandbox::new().unwrap();
    let t = scratch("tmp2");
    let policy = base(Prelude::Windows, &t, None, None).starting_in(&t);
    println!(
        "BEFORE type nul: {}",
        run(&sb, "cmd.exe", &["/c", "type nul"], &policy)
    );
    println!(
        "ADD AAP generic rw on NUL: {}",
        add_ace(r"\\.\NUL", "S-1-15-2-1", 0xC0000000)
    );
    println!("SDDL after: {}", sddl_of(r"\\.\NUL"));
    println!(
        "AFTER type nul: {}",
        run(&sb, "cmd.exe", &["/c", "type nul"], &policy)
    );
    println!("AFTER git: {}", run(&sb, "git", &["--version"], &policy));
}
