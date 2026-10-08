"""Break the computer host's guards on purpose; a mutant is KILLED only if it compiled, a control passed first, and a test then failed."""
import os
import subprocess
import sys

ROOT = "/Users/mohsin/projects/quansiov3/.claude/worktrees/agent-a0a3b7df048003b48"
SP = "/private/tmp/claude-501/-Users-mohsin-projects-quansiov3/102fe2d6-cef3-4453-807d-4a7bfb79bf22/scratchpad"
OUT = sys.argv[1]
HOST = ROOT + "/crates/server/src/computer/host.rs"
NOTIFY = ROOT + "/crates/server/src/notify/notifier.rs"
STORE = ROOT + "/crates/server/src/runtime/state_machine/store.rs"
LS = ROOT + "/crates/server/src/api/live_sessions.rs"

MUTANTS = [
    ("a missing permission is returned as a plain failure, not a park", HOST,
     "SequenceEnd::Parked(attention) => Ok(suspend(attention)),",
     'SequenceEnd::Parked(attention) => Ok(failure("CONFLICT_STATE", "m", attention.code(), true)),', "computer_host"),
    ("a run that is not running still sends input", HOST,
     "if run.status != RunStatus::Running {", "if false && run.status != RunStatus::Running {", "computer_host"),
    ("an unverified receipt settles as success", HOST,
     "LedgerOutcome::OutcomeUnknown => Ok(HostOutcome::Unknown {",
     "LedgerOutcome::OutcomeUnknown => Ok(HostOutcome::Success { output: json!({}), evidence_ids: Vec::new(), remote_ref: None }), LedgerOutcome::SettledFailed if false => Ok(HostOutcome::Unknown {", "computer_host"),
    ("the attention item is dropped from run.suspended", STORE,
     'payload["attention"] = attention;', "let _ = attention;", "computer_host"),
    ("the blocked notification is never made", NOTIFY,
     '"run.suspended" if payload.get("attention").is_some_and(Value::is_object) => {',
     '"run.suspended" if false && payload.get("attention").is_some_and(Value::is_object) => {', "computer_host"),
    ("a service principal may decide a certificate", LS,
     "    if scope.principal.is_some() {\n        return Err(ApiError::new(\n            ApiErrorCode::ScopeForbidden,\n            format!(\"{command} is a person's decision",
     "    if false && scope.principal.is_some() {\n        return Err(ApiError::new(\n            ApiErrorCode::ScopeForbidden,\n            format!(\"{command} is a person's decision", "live_sessions"),
    ("a viewer may decide a certificate", LS,
     "        ActionFamily::ApproveEffect,\n        correlation,\n    )\n    .await?;\n    Ok(DecisionRequest {",
     "        ActionFamily::Read,\n        correlation,\n    )\n    .await?;\n    Ok(DecisionRequest {", "live_sessions"),
]


def cargo(test):
    env = dict(os.environ)
    for line in open(SP + "/env.sh"):
        if line.startswith("export "):
            k, v = line[7:].strip().split("=", 1)
            env[k] = v.strip('"')
    r = subprocess.run(["cargo", "test", "-p", "quansio-server", "--test", test], cwd=ROOT, env=env, capture_output=True, text=True)
    return r.returncode, r.stdout + r.stderr


log = open(OUT, "w")


def say(text):
    log.write(text + "\n")
    log.flush()


for name, path, old, new, test in MUTANTS:
    original = open(path).read()
    if old not in original:
        say(f"NO-VERDICT  {name}: pattern not found")
        continue
    rc, _ = cargo(test)
    if rc != 0:
        say(f"NO-VERDICT  {name}: control failed")
        continue
    say(f"CONTROL-PASSED  {name}")
    try:
        open(path, "w").write(original.replace(old, new, 1))
        build = subprocess.run(["cargo", "test", "-p", "quansio-server", "--test", test, "--no-run"], cwd=ROOT, capture_output=True, text=True,
                               env={**os.environ, **{k: v.strip('"') for k, v in (l[7:].strip().split("=", 1) for l in open(SP + "/env.sh") if l.startswith("export "))}})
        if build.returncode != 0:
            say(f"NO-VERDICT  {name}: does not compile")
            continue
        say(f"COMPILES  {name}")
        rc, out = cargo(test)
        failed = [l for l in out.splitlines() if l.startswith("test ") and l.endswith("FAILED")]
        say(("KILLED  " if rc != 0 and failed else "SURVIVED  ") + name + (" :: " + failed[0] if failed else ""))
    finally:
        open(path, "w").write(original)
say("DONE")
