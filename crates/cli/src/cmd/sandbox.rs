use std::process::Command;
use std::time::{Duration, Instant};

use steward_core::Ctx;
use steward_sandbox::detect;

use crate::{Result, SandboxCmd, SandboxRunArgs};

pub fn sandbox(ctx: &Ctx, c: SandboxCmd) -> Result {
    match c {
        SandboxCmd::Check { dir } => {
            let Some(backend) = detect() else {
                println!("no sandbox backend (needs sandbox-exec on macOS, bwrap on Linux)");
                return Ok(());
            };
            println!("backend   {backend}");
            let sb = ctx.sandbox(&dir, false, &[], &[])?;
            let p = sb.policy();
            println!("network   {}", if p.network { "allowed" } else { "denied" });
            for w in &p.writable {
                println!("writable  {}", w.display());
            }
            let hidden: Vec<_> = p.hidden.iter().filter(|h| h.exists()).collect();
            for h in hidden {
                println!("hidden    {}", h.display());
            }
            let mut env: Vec<&str> = steward_sandbox::BASE_ENV.to_vec();
            env.extend(p.pass_env.iter().map(String::as_str));
            println!("env       {} TMPDIR", env.join(" "));
            Ok(())
        }
        SandboxCmd::Run(a) => run(ctx, a),
    }
}

fn run(ctx: &Ctx, a: SandboxRunArgs) -> Result {
    let sb = ctx.sandbox(&a.dir, a.net, &a.write, &a.env)?;
    let (program, args) = a.command.split_first().expect("clap requires a command");
    let mut cmd: Command = sb.command(program, args, &a.dir);
    let mut child = cmd.spawn()?;
    let start = Instant::now();
    let status = loop {
        if let Some(st) = child.try_wait()? {
            break st;
        }
        if a.timeout
            .is_some_and(|t| start.elapsed() >= Duration::from_secs(t))
        {
            child.kill()?;
            child.wait()?;
            return Err(format!("timed out after {}s", a.timeout.unwrap_or(0)).into());
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    drop(sb);
    if status.success() {
        Ok(())
    } else {
        Err(format!("{program}: {status}").into())
    }
}
