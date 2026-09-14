//! Allowlisted Windows UI Automation/input broker for EXEC-010.
//!
//! The executable text is fixed in this crate. ToolCall values cross only as base64 environment values,
//! so an application name, text payload, or key token can never become a PowerShell command. Rust machine
//! authority has already applied capability, policy, approval and durable takeover fencing before calling
//! this bridge; the bridge rechecks foreground identity immediately before the UIA/input operation.

#[cfg(windows)]
use std::process::Command;

use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use quansio_machine::computer_use::bridge::{
    NativeBridgeError, NativeComputerAction, NativeComputerBridge,
};
use quansio_machine::computer_use::AppIdentity;
use serde_json::Value;

/// The Windows implementation of the native computer port.
#[derive(Debug, Clone, Copy, Default)]
pub struct WindowsComputerBridge;

impl NativeComputerBridge for WindowsComputerBridge {
    fn foreground_app(&self) -> Result<AppIdentity, NativeBridgeError> {
        let value = invoke(IDENTITY_SCRIPT, &[])?;
        let bundle_id = required_string(&value, "bundle_id")?;
        let name = required_string(&value, "name")?;
        let pid = value
            .get("pid")
            .and_then(Value::as_i64)
            .and_then(|pid| i32::try_from(pid).ok())
            .filter(|pid| *pid > 0)
            .ok_or_else(|| NativeBridgeError::new("windows_identity_pid_invalid"))?;
        Ok(AppIdentity::new(bundle_id, name, pid))
    }

    fn execute(&self, action: &NativeComputerAction) -> Result<Value, NativeBridgeError> {
        let mut values = vec![("QUANSIO_EXPECTED_APP", encoded(action.application()))];
        match action {
            NativeComputerAction::Read {
                max_nodes,
                max_depth,
                include_screenshot,
                ..
            } => {
                if *max_nodes == 0 || *max_nodes > 512 || *max_depth > 16 {
                    return Err(NativeBridgeError::new("windows_read_bounds_invalid"));
                }
                values.extend([
                    ("QUANSIO_OPERATION", "read".to_string()),
                    ("QUANSIO_MAX_NODES", max_nodes.to_string()),
                    ("QUANSIO_MAX_DEPTH", max_depth.to_string()),
                    (
                        "QUANSIO_SCREENSHOT",
                        if *include_screenshot { "1" } else { "0" }.to_string(),
                    ),
                ]);
            }
            NativeComputerAction::Click { x, y, button, .. } => {
                if !matches!(button.as_str(), "left" | "right" | "middle") {
                    return Err(NativeBridgeError::new("windows_mouse_button_invalid"));
                }
                values.extend([
                    ("QUANSIO_OPERATION", "click".to_string()),
                    ("QUANSIO_X", x.to_string()),
                    ("QUANSIO_Y", y.to_string()),
                    ("QUANSIO_BUTTON", button.clone()),
                ]);
            }
            NativeComputerAction::Type { text, .. } => values.extend([
                ("QUANSIO_OPERATION", "type".to_string()),
                ("QUANSIO_TEXT", encoded(text)),
            ]),
            NativeComputerAction::Clipboard {
                operation, text, ..
            } => {
                if !matches!(operation.as_str(), "read" | "write") {
                    return Err(NativeBridgeError::new(
                        "windows_clipboard_operation_invalid",
                    ));
                }
                values.extend([
                    ("QUANSIO_OPERATION", format!("clipboard_{operation}")),
                    ("QUANSIO_TEXT", encoded(text)),
                ]);
            }
            NativeComputerAction::SystemKey { keys, .. } => {
                validate_chord(keys)?;
                values.extend([
                    ("QUANSIO_OPERATION", "system_key".to_string()),
                    ("QUANSIO_KEYS", keys.join(",")),
                ]);
            }
        }
        let refs: Vec<(&str, &str)> = values
            .iter()
            .map(|(key, value)| (*key, value.as_str()))
            .collect();
        invoke(COMPUTER_SCRIPT, &refs)
    }
}

fn required_string(value: &Value, field: &str) -> Result<String, NativeBridgeError> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .ok_or_else(|| NativeBridgeError::new(format!("windows_identity_{field}_invalid")))
}

fn encoded(value: &str) -> String {
    STANDARD.encode(value.as_bytes())
}

fn validate_chord(keys: &[String]) -> Result<(), NativeBridgeError> {
    const MODIFIERS: &[&str] = &["control", "option", "shift"];
    const PRIMARY: &[&str] = &[
        "a", "c", "v", "x", "z", "return", "tab", "space", "delete", "escape", "left", "right",
        "down", "up",
    ];
    if !(1..=4).contains(&keys.len()) {
        return Err(NativeBridgeError::new("windows_key_chord_size_invalid"));
    }
    let mut primary = 0;
    for key in keys {
        let key = key.to_ascii_lowercase();
        if PRIMARY.contains(&key.as_str()) {
            primary += 1;
        } else if !MODIFIERS.contains(&key.as_str()) {
            return Err(NativeBridgeError::new("windows_key_chord_token_invalid"));
        }
    }
    if primary != 1 {
        return Err(NativeBridgeError::new("windows_key_chord_primary_invalid"));
    }
    Ok(())
}

#[cfg(not(windows))]
fn invoke(_script: &str, _values: &[(&str, &str)]) -> Result<Value, NativeBridgeError> {
    Err(NativeBridgeError::new(
        "BLOCKED_EXTERNAL: QUANSIO_TEST_WINDOWS_COMPUTER_USE=1 requires a Windows 11 interactive host",
    ))
}

#[cfg(windows)]
fn invoke(script: &str, values: &[(&str, &str)]) -> Result<Value, NativeBridgeError> {
    if std::env::var("QUANSIO_TEST_WINDOWS_COMPUTER_USE").as_deref() != Ok("1") {
        return Err(NativeBridgeError::new(
            "BLOCKED_EXTERNAL: QUANSIO_TEST_WINDOWS_COMPUTER_USE=1 is required",
        ));
    }
    let mut command = Command::new("powershell.exe");
    command.args([
        "-NoLogo",
        "-NoProfile",
        "-NonInteractive",
        "-Command",
        script,
    ]);
    for (key, value) in values {
        command.env(key, value);
    }
    let output = command
        .output()
        .map_err(|_| NativeBridgeError::new("windows_broker_launch_failed"))?;
    if !output.status.success() {
        return Err(NativeBridgeError::new("windows_broker_operation_failed"));
    }
    serde_json::from_slice(&output.stdout)
        .map_err(|_| NativeBridgeError::new("windows_broker_output_invalid"))
}

// Fixed scripts only. No caller-controlled value is interpolated into either string.
const IDENTITY_SCRIPT: &str = r#"
$ErrorActionPreference='Stop'
Add-Type @'
using System; using System.Runtime.InteropServices;
public static class QuansioWin32 {
 [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
 [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint p);
}
'@
$h=[QuansioWin32]::GetForegroundWindow(); if($h -eq [IntPtr]::Zero){throw 'no_foreground_app'}
[uint32]$pidValue=0; [void][QuansioWin32]::GetWindowThreadProcessId($h,[ref]$pidValue)
$p=Get-Process -Id $pidValue -ErrorAction Stop; $identity=$p.Path
if([string]::IsNullOrWhiteSpace($identity)){throw 'unidentified_foreground_app'}
@{bundle_id=$identity.ToLowerInvariant();name=$p.ProcessName;pid=[int]$pidValue}|ConvertTo-Json -Compress
"#;

const COMPUTER_SCRIPT: &str = r#"
$ErrorActionPreference='Stop'
Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName System.Windows.Forms
Add-Type @'
using System; using System.Runtime.InteropServices;
public static class QuansioWin32 {
 [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
 [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint p);
 [DllImport("user32.dll")] public static extern bool SetCursorPos(int x,int y);
 [DllImport("user32.dll")] public static extern void mouse_event(uint f,uint x,uint y,uint d,UIntPtr e);
}
'@
function Decode([string]$v){[Text.Encoding]::UTF8.GetString([Convert]::FromBase64String($v))}
$h=[QuansioWin32]::GetForegroundWindow(); if($h -eq [IntPtr]::Zero){throw 'no_foreground_app'}
[uint32]$pidValue=0; [void][QuansioWin32]::GetWindowThreadProcessId($h,[ref]$pidValue)
$p=Get-Process -Id $pidValue -ErrorAction Stop; $actual=$p.Path.ToLowerInvariant()
$expected=(Decode $env:QUANSIO_EXPECTED_APP).ToLowerInvariant()
if($actual -ne $expected){throw 'foreground_app_changed'}
$op=$env:QUANSIO_OPERATION
switch($op){
 'read' {
  $max=[int]$env:QUANSIO_MAX_NODES; $depthMax=[int]$env:QUANSIO_MAX_DEPTH
  $root=[System.Windows.Automation.AutomationElement]::FromHandle($h)
  $queue=New-Object System.Collections.Queue; $queue.Enqueue(@($root,0)); $nodes=@(); $truncated=$false
  while($queue.Count -gt 0){
   $item=$queue.Dequeue(); $el=$item[0]; $depth=[int]$item[1]
   if($nodes.Count -ge $max){$truncated=$true;break}
   $nodes+=@{role=$el.Current.ControlType.ProgrammaticName;title=$el.Current.Name;value='';depth=$depth;child_count=0}
   if($depth -lt $depthMax){
    $walker=[System.Windows.Automation.TreeWalker]::ControlViewWalker; $child=$walker.GetFirstChild($el)
    while($null -ne $child){$queue.Enqueue(@($child,$depth+1));$child=$walker.GetNextSibling($child)}
   }
  }
  @{application=$actual;nodes=$nodes;truncated=$truncated}|ConvertTo-Json -Compress -Depth 8
 }
 'click' {
  [void][QuansioWin32]::SetCursorPos([int]$env:QUANSIO_X,[int]$env:QUANSIO_Y)
  $flags=@{left=@(2,4);right=@(8,16);middle=@(32,64)}[$env:QUANSIO_BUTTON]
  if($null -eq $flags){throw 'invalid_button'}
  [QuansioWin32]::mouse_event($flags[0],0,0,0,[UIntPtr]::Zero)
  [QuansioWin32]::mouse_event($flags[1],0,0,0,[UIntPtr]::Zero)
  @{clicked=$true}|ConvertTo-Json -Compress
 }
 'type' {
  $focused=[System.Windows.Automation.AutomationElement]::FocusedElement
  $pattern=$null
  if(-not $focused.TryGetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern,[ref]$pattern)){throw 'focused_element_not_editable'}
  ([System.Windows.Automation.ValuePattern]$pattern).SetValue((Decode $env:QUANSIO_TEXT))
  @{typed=$true}|ConvertTo-Json -Compress
 }
 'clipboard_read' {@{text=(Get-Clipboard -Raw);written=$false}|ConvertTo-Json -Compress}
 'clipboard_write' {Set-Clipboard -Value (Decode $env:QUANSIO_TEXT);@{written=$true}|ConvertTo-Json -Compress}
 'system_key' {
  $mods='';$primary=$null
  foreach($key in $env:QUANSIO_KEYS.Split(',')){
   switch($key){'control'{$mods+='^'}'option'{$mods+='%'}'shift'{$mods+='+'}'return'{$primary='{ENTER}'}'tab'{$primary='{TAB}'}'space'{$primary=' '}'delete'{$primary='{DEL}'}'escape'{$primary='{ESC}'}'left'{$primary='{LEFT}'}'right'{$primary='{RIGHT}'}'down'{$primary='{DOWN}'}'up'{$primary='{UP}'}default{$primary=$key}}
  }
  [System.Windows.Forms.SendKeys]::SendWait($mods+$primary);@{sent=$true}|ConvertTo-Json -Compress
 }
 default {throw 'operation_not_allowlisted'}
}
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chord_and_operation_values_are_allowlisted_before_the_platform() {
        assert!(validate_chord(&["control".into(), "a".into()]).is_ok());
        assert!(validate_chord(&["launch".into()]).is_err());
        assert!(validate_chord(&["control".into(), "shift".into()]).is_err());
        assert!(!COMPUTER_SCRIPT.contains("QUANSIO_TEXT}"));
        assert!(COMPUTER_SCRIPT.contains("Decode $env:QUANSIO_TEXT"));
    }

    #[test]
    fn non_windows_boundary_reports_blocked_external() {
        #[cfg(not(windows))]
        let error = WindowsComputerBridge
            .foreground_app()
            .expect_err("not Windows");
        #[cfg(not(windows))]
        eprintln!("{}", error.code);
        #[cfg(not(windows))]
        assert!(error.code.contains("BLOCKED_EXTERNAL"));
    }

    #[test]
    fn caller_text_is_encoded_not_made_executable() {
        let payload = "'; Remove-Item C:\\\\*; '";
        let encoded = encoded(payload);
        assert!(!encoded.contains("Remove-Item"));
        assert_eq!(
            STANDARD.decode(encoded).expect("base64"),
            payload.as_bytes()
        );
    }
}
