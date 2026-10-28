//! Mint an E5-1 JWT for the BPM engine REST API (operator/test helper).
//!
//! Reads the signing secret from `BPM_JWT_SECRET` (the same value the
//! server gets from `auth.jwt_secret` / `BPM_AUTH__JWT_SECRET`) and prints
//! an HS256 token with the claims `{ sub, tenant, role, exp }` to stdout.
//!
//! Usage:
//!
//! ```sh
//! cargo run -p bpm-engine-server-rest --example mint_jwt -- \
//!     --sub alice --tenant acme --role operator [--exp 1735689600 | --exp-in 3600]
//! ```
//!
//! `--exp` is an absolute unix-seconds expiry; `--exp-in` is seconds from
//! now (default 3600). Roles: deployer | operator | worker | auditor. Send
//! the printed token as `Authorization: Bearer <token>` on `/api/v1`
//! requests — see `docs/rbac.md` for the role→permission matrix, the
//! tenant semantics, and the X-Tenant-Id deprecation.

use bpm_engine_server_rest::middleware::mint_jwt;
use std::time::{SystemTime, UNIX_EPOCH};

const USAGE: &str = "Usage:
  mint_jwt --sub <actor> --tenant <tenant> --role <deployer|operator|worker|auditor> \
[--exp <unix-secs> | --exp-in <secs-from-now>]

Options:
  --sub      Actor (subject) claim, e.g. \"alice\"     (required)
  --tenant   Tenant claim, e.g. \"acme\"                 (required, non-empty)
  --role     deployer | operator | worker | auditor     (required)
  --exp      Absolute expiry, unix seconds              (default: none)
  --exp-in   Expiry seconds from now                    (default: 3600; ignored \
when --exp is given)
  --help     Show this usage

The signing secret comes from BPM_JWT_SECRET (the same value the server reads \
from auth.jwt_secret / BPM_AUTH__JWT_SECRET). Send the printed token as \
\"Authorization: Bearer <token>\" on /api/v1 requests.";

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before the unix epoch")
        .as_secs()
}

fn next_value(raw: &mut std::env::Args, name: &str) -> Result<String, String> {
    raw.next()
        .ok_or_else(|| format!("missing value for {name} (see --help)"))
}

fn parse_u64(value: &str, flag: &str) -> Result<u64, String> {
    value
        .parse()
        .map_err(|_| format!("{flag}: {value:?} is not a seconds integer"))
}

struct Args {
    sub: Option<String>,
    tenant: Option<String>,
    role: Option<String>,
    exp: Option<u64>,
    exp_in: Option<u64>,
}

fn parse_args(raw: &mut std::env::Args) -> Result<Args, String> {
    let mut args = Args {
        sub: None,
        tenant: None,
        role: None,
        exp: None,
        exp_in: None,
    };
    while let Some(flag) = raw.next() {
        match flag.as_str() {
            "--sub" => args.sub = Some(next_value(raw, "--sub")?),
            "--tenant" => args.tenant = Some(next_value(raw, "--tenant")?),
            "--role" => args.role = Some(next_value(raw, "--role")?),
            "--exp" => {
                let value = next_value(raw, "--exp")?;
                args.exp = Some(parse_u64(&value, "--exp")?);
            }
            "--exp-in" => {
                let value = next_value(raw, "--exp-in")?;
                args.exp_in = Some(parse_u64(&value, "--exp-in")?);
            }
            "--help" | "-h" => return Err(String::new()),
            other => return Err(format!("unknown argument {other:?} (see --help)")),
        }
    }
    Ok(args)
}

fn fail(message: &str) -> ! {
    eprintln!("{message}\n\n{USAGE}");
    std::process::exit(1);
}

fn main() {
    let mut raw = std::env::args();
    // Discard argv[0]: only the flags are arguments.
    let _program_name = raw.next();
    let args = match parse_args(&mut raw) {
        Ok(args) => args,
        // "" is the --help exit: print usage and succeed.
        Err(message) if message.is_empty() => {
            println!("{USAGE}");
            return;
        }
        Err(message) => fail(&message),
    };

    let Some(sub) = args.sub else {
        fail("--sub is required");
    };
    let Some(tenant) = args.tenant else {
        fail("--tenant is required");
    };
    if tenant.is_empty() {
        fail("--tenant must be a non-empty tenant id");
    }
    let Some(role) = args.role else {
        fail("--role is required");
    };

    let secret = std::env::var("BPM_JWT_SECRET").unwrap_or_default();
    if secret.is_empty() {
        eprintln!(
            "BPM_JWT_SECRET is not set; export the same secret the server uses \
             (auth.jwt_secret / BPM_AUTH__JWT_SECRET)"
        );
        std::process::exit(1);
    }

    let exp = args
        .exp
        .unwrap_or_else(|| now_secs() + args.exp_in.unwrap_or(3600));

    match mint_jwt(&secret, &sub, &tenant, &role, exp) {
        Ok(token) => println!("{token}"),
        Err(err) => {
            eprintln!("{err}");
            std::process::exit(1);
        }
    }
}
