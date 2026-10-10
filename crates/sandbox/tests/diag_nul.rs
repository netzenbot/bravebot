#![cfg(windows)]
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
