//! Windows UI Automation inspection and explicit local execution.
#![cfg(target_os = "windows")]

use crate::{Assertion, AutomationError, Backend, Rect, Selector, WindowTarget};
use std::path::Path;
use windows::core::{BOOL, BSTR, PWSTR};
use windows::Win32::UI::Accessibility::{
    ExpandCollapseState_Expanded, IUIAutomationExpandCollapsePattern,
    IUIAutomationSelectionItemPattern, IUIAutomationValuePattern, UIA_ExpandCollapsePatternId,
    UIA_SelectionItemPatternId, UIA_ValuePatternId,
};
use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, IsIconic, SetForegroundWindow};
use windows::Win32::{
    Foundation::{CloseHandle, HWND, LPARAM},
    System::{
        Com::{
            CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER,
            COINIT_APARTMENTTHREADED,
        },
        Threading::{
            OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_FORMAT,
            PROCESS_QUERY_LIMITED_INFORMATION,
        },
    },
    UI::{
        Accessibility::{
            CUIAutomation, IUIAutomation, IUIAutomationElement, TreeScope_Descendants,
            UIA_ButtonControlTypeId, UIA_ComboBoxControlTypeId, UIA_EditControlTypeId,
            UIA_ListItemControlTypeId, UIA_MenuControlTypeId, UIA_MenuItemControlTypeId,
            UIA_CONTROLTYPE_ID,
        },
        WindowsAndMessaging::{
            EnumWindows, GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId,
            IsWindowVisible,
        },
    },
};

#[derive(Debug, Clone)]
pub struct ControlInfo {
    pub role: String,
    pub name: String,
    pub automation_id: String,
}

fn text(hwnd: HWND) -> String {
    let len = unsafe { GetWindowTextLengthW(hwnd) };
    if !(1..=32767).contains(&len) {
        return String::new();
    }
    let mut buf = vec![0u16; len as usize + 1];
    let n = unsafe { GetWindowTextW(hwnd, &mut buf) };
    if n != unsafe { GetWindowTextLengthW(hwnd) } {
        return String::new();
    }
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}
fn exe(hwnd: HWND) -> String {
    let mut pid = 0u32;
    unsafe {
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
    }
    if pid == 0 {
        return String::new();
    }
    let Ok(process) = (unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) })
    else {
        return String::new();
    };
    let mut buf = [0u16; 1024];
    let mut len = buf.len() as u32;
    let result = unsafe {
        QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_FORMAT(0),
            PWSTR(buf.as_mut_ptr()),
            &mut len,
        )
    };
    unsafe {
        let _ = CloseHandle(process);
    }
    if result.is_ok() {
        Path::new(&String::from_utf16_lossy(&buf[..len as usize]))
            .file_name()
            .map(|x| x.to_string_lossy().into_owned())
            .unwrap_or_default()
    } else {
        String::new()
    }
}
unsafe extern "system" fn collect(hwnd: HWND, data: LPARAM) -> BOOL {
    let out = &mut *(data.0 as *mut Vec<HWND>);
    if IsWindowVisible(hwnd).as_bool() {
        out.push(hwnd);
    }
    BOOL(1)
}
fn windows() -> Vec<HWND> {
    let mut out = Vec::new();
    unsafe {
        let _ = EnumWindows(Some(collect), LPARAM(&mut out as *mut _ as isize));
    }
    out
}
#[allow(non_upper_case_globals)]
fn role(control: UIA_CONTROLTYPE_ID) -> String {
    match control {
        UIA_ButtonControlTypeId => "button",
        UIA_EditControlTypeId => "edit",
        UIA_ComboBoxControlTypeId => "combo_box",
        UIA_ListItemControlTypeId => "list_item",
        UIA_MenuControlTypeId => "menu",
        UIA_MenuItemControlTypeId => "menu_item",
        _ => "unknown",
    }
    .into()
}
fn bstr(v: windows::core::Result<BSTR>) -> String {
    v.map(|x| x.to_string()).unwrap_or_default()
}

/// Finds exactly one visible top-level window by exact executable basename and title,
/// then returns only UIA role/name/automation id. It never reads Value/Text patterns.
pub fn inspect(executable: &str, title: &str) -> Result<Vec<ControlInfo>, String> {
    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED)
            .ok()
            .map_err(|e| e.to_string())?;
    }
    let result = (|| {
        let matches: Vec<_> = windows()
            .into_iter()
            .filter(|h| text(*h) == title && exe(*h).eq_ignore_ascii_case(executable))
            .collect();
        if matches.len() != 1 {
            return Err(format!(
                "expected one matching window, found {}",
                matches.len()
            ));
        }
        let automation: IUIAutomation =
            unsafe { CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER) }
                .map_err(|e| e.to_string())?;
        let root: IUIAutomationElement =
            unsafe { automation.ElementFromHandle(matches[0]) }.map_err(|e| e.to_string())?;
        let condition = unsafe { automation.CreateTrueCondition() }.map_err(|e| e.to_string())?;
        let all = unsafe { root.FindAll(TreeScope_Descendants, &condition) }
            .map_err(|e| e.to_string())?;
        let mut result = Vec::new();
        let count = unsafe { all.Length() }.map_err(|e| e.to_string())?;
        if count > 512 {
            return Err(format!(
                "UIA tree has {count} descendants; refusing capped inspection"
            ));
        }
        for i in 0..count {
            let e = unsafe { all.GetElement(i) }.map_err(|e| e.to_string())?;
            let control_role = role(unsafe { e.CurrentControlType() }.map_err(|e| e.to_string())?);
            if control_role == "unknown" {
                continue;
            }
            result.push(ControlInfo {
                role: control_role,
                name: bstr(unsafe { e.CurrentName() }),
                automation_id: bstr(unsafe { e.CurrentAutomationId() }),
            });
        }
        Ok(result)
    })();
    unsafe {
        CoUninitialize();
    }
    result
}

struct ComApartment(std::marker::PhantomData<std::rc::Rc<()>>);
impl ComApartment {
    fn new() -> Result<Self, AutomationError> {
        unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok() }.map_err(native_error)?;
        Ok(Self(std::marker::PhantomData))
    }
}
impl Drop for ComApartment {
    fn drop(&mut self) {
        unsafe { CoUninitialize() };
    }
}
fn native_error(e: windows::core::Error) -> AutomationError {
    // Provider error strings may include application data. Keep only the error code.
    AutomationError::Target(format!("UIA error {:?}", e.code()))
}
fn refused(message: &str) -> AutomationError {
    AutomationError::Target(message.into())
}

struct DesktopLease(windows::Win32::Foundation::HANDLE);
impl DesktopLease {
    fn acquire() -> Result<Self, AutomationError> {
        use windows::{
            core::w,
            Win32::{
                Foundation::{WAIT_ABANDONED, WAIT_OBJECT_0},
                System::Threading::{CreateMutexW, WaitForSingleObject},
            },
        };
        let handle = unsafe { CreateMutexW(None, false, w!("Local\\SyncClipAutomationP1")) }
            .map_err(native_error)?;
        let wait = unsafe { WaitForSingleObject(handle, 0) };
        if wait != WAIT_OBJECT_0 && wait != WAIT_ABANDONED {
            unsafe {
                let _ = CloseHandle(handle);
            }
            return Err(refused("another local automation flow owns the desktop"));
        }
        Ok(Self(handle))
    }
}
impl Drop for DesktopLease {
    fn drop(&mut self) {
        unsafe {
            let _ = windows::Win32::System::Threading::ReleaseMutex(self.0);
            let _ = CloseHandle(self.0);
        }
    }
}

/// A synchronous, thread-affine adapter. It never invokes arbitrary buttons,
/// sends keys, modifies the clipboard, or clears an existing draft.
pub struct WindowsBackend {
    automation: IUIAutomation,
    root: Option<IUIAutomationElement>,
    target: Option<WindowTarget>,
    hwnd: HWND,
    focused: Option<Selector>,
    lease: Option<DesktopLease>,
    // Declared last so all COM interfaces are released before CoUninitialize.
    _com: ComApartment,
}
impl WindowsBackend {
    pub fn new() -> Result<Self, AutomationError> {
        let com = ComApartment::new()?;
        let automation = unsafe { CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER) }
            .map_err(native_error)?;
        Ok(Self {
            automation,
            root: None,
            target: None,
            hwnd: HWND::default(),
            focused: None,
            lease: None,
            _com: com,
        })
    }
    fn unique_window(target: &WindowTarget) -> Result<HWND, AutomationError> {
        let candidates: Vec<_> = windows()
            .into_iter()
            .filter(|h| text(*h) == target.title && exe(*h).eq_ignore_ascii_case(&target.exe))
            .collect();
        if candidates.len() != 1 {
            return Err(refused("target must match exactly one visible window"));
        }
        Ok(candidates[0])
    }
    fn check_target(&self, foreground: bool) -> Result<(), AutomationError> {
        let target = self
            .target
            .as_ref()
            .ok_or_else(|| refused("target not resolved"))?;
        if Self::unique_window(target)? != self.hwnd {
            return Err(refused("target window changed"));
        }
        if foreground
            && unsafe { IsIconic(self.hwnd).as_bool() || GetForegroundWindow() != self.hwnd }
        {
            return Err(refused(
                "target is minimized or lost foreground; execution paused",
            ));
        }
        Ok(())
    }
    fn within(
        element: &IUIAutomationElement,
        region: Option<Rect>,
    ) -> Result<bool, AutomationError> {
        let Some(region) = region else {
            return Ok(true);
        };
        let r = unsafe { element.CurrentBoundingRectangle() }.map_err(native_error)?;
        Ok(r.left >= region.left
            && r.top >= region.top
            && r.right <= region.right
            && r.bottom <= region.bottom)
    }
    fn element(&self, selector: &Selector) -> Result<IUIAutomationElement, AutomationError> {
        self.check_target(false)?;
        let root = self
            .root
            .as_ref()
            .ok_or_else(|| refused("target not resolved"))?;
        let condition = unsafe { self.automation.CreateTrueCondition() }.map_err(native_error)?;
        let all =
            unsafe { root.FindAll(TreeScope_Descendants, &condition) }.map_err(native_error)?;
        let count = unsafe { all.Length() }.map_err(native_error)?;
        if count > 512 {
            return Err(refused(
                "UIA tree exceeds 512 elements; refusing truncated selection",
            ));
        }
        let mut matched = None;
        for i in 0..count {
            let e = unsafe { all.GetElement(i) }.map_err(native_error)?;
            if role(unsafe { e.CurrentControlType() }.map_err(native_error)?) != selector.role {
                continue;
            }
            if let Some(name) = &selector.name {
                if unsafe { e.CurrentName() }.map_err(native_error)? != *name {
                    continue;
                }
            }
            if let Some(id) = &selector.automation_id {
                if unsafe { e.CurrentAutomationId() }.map_err(native_error)? != *id {
                    continue;
                }
            }
            if !Self::within(&e, selector.region)?
                || !Self::within(&e, self.target.as_ref().and_then(|t| t.region))?
            {
                continue;
            }
            if matched.is_some() {
                return Err(refused("selector matches multiple controls"));
            }
            matched = Some(e);
        }
        matched.ok_or_else(|| refused("selector matched no control"))
    }
    fn usable(&self, e: &IUIAutomationElement) -> Result<(), AutomationError> {
        self.check_target(true)?;
        if unsafe { e.CurrentIsPassword() }
            .map_err(native_error)?
            .as_bool()
            || !unsafe { e.CurrentIsEnabled() }
                .map_err(native_error)?
                .as_bool()
            || unsafe { e.CurrentIsOffscreen() }
                .map_err(native_error)?
                .as_bool()
        {
            return Err(refused("control is protected, disabled, or offscreen"));
        }
        Ok(())
    }
    fn value(e: &IUIAutomationElement) -> Result<IUIAutomationValuePattern, AutomationError> {
        unsafe { e.GetCurrentPatternAs(UIA_ValuePatternId) }.map_err(|_| {
            AutomationError::Unsupported("control does not expose ValuePattern".into())
        })
    }
    fn verify_focus(&self, e: &IUIAutomationElement) -> Result<(), AutomationError> {
        self.usable(e)?;
        let focused = unsafe { self.automation.GetFocusedElement() }.map_err(native_error)?;
        if !unsafe { self.automation.CompareElements(e, &focused) }
            .map_err(native_error)?
            .as_bool()
            || !unsafe { e.CurrentHasKeyboardFocus() }
                .map_err(native_error)?
                .as_bool()
        {
            return Err(refused("input control does not own keyboard focus"));
        }
        Ok(())
    }
}
impl Backend for WindowsBackend {
    fn resolve_target(&mut self, target: &WindowTarget) -> Result<(), AutomationError> {
        if self.lease.is_none() {
            self.lease = Some(DesktopLease::acquire()?);
        }
        self.focused = None;
        let hwnd = Self::unique_window(target)?;
        let root = unsafe { self.automation.ElementFromHandle(hwnd) }.map_err(native_error)?;
        self.root = Some(root);
        self.hwnd = hwnd;
        self.target = Some(target.clone());
        Ok(())
    }
    fn focus(&mut self, selector: &Selector) -> Result<(), AutomationError> {
        self.focused = None;
        let e = self.element(selector)?;
        if unsafe { IsIconic(self.hwnd) }.as_bool() {
            return Err(refused("restore target window before executing"));
        }
        if unsafe { GetForegroundWindow() } != self.hwnd
            && !unsafe { SetForegroundWindow(self.hwnd) }.as_bool()
        {
            return Err(refused("Windows refused foreground activation"));
        }
        // Cross-thread foreground activation can settle after SetForegroundWindow
        // returns. Wait for this one request; never repeatedly steal focus.
        for _ in 0..20 {
            if unsafe { GetForegroundWindow() } == self.hwnd {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        self.usable(&e)?;
        unsafe { e.SetFocus() }.map_err(native_error)?;
        self.verify_focus(&e)?;
        self.focused = Some(selector.clone());
        Ok(())
    }
    fn input_text(&mut self, input: &str) -> Result<(), AutomationError> {
        if input.chars().any(char::is_control) {
            return Err(refused(
                "P1 input refuses control characters including newline and tab",
            ));
        }
        let selector = self
            .focused
            .as_ref()
            .ok_or_else(|| refused("explicit focus step is required"))?;
        if selector.role != "edit" {
            return Err(refused("input requires edit control"));
        }
        let e = self.element(selector)?;
        self.verify_focus(&e)?;
        let value = Self::value(&e)?;
        if unsafe { value.CurrentIsReadOnly() }
            .map_err(native_error)?
            .as_bool()
        {
            return Err(refused("input is read-only"));
        }
        if !unsafe { value.CurrentValue() }
            .map_err(native_error)?
            .is_empty()
        {
            return Err(refused("existing draft preserved; input must be empty"));
        }
        self.verify_focus(&e)?;
        unsafe { value.SetValue(&BSTR::from(input)) }.map_err(native_error)?;
        if unsafe { value.CurrentValue() }.map_err(native_error)? != input {
            return Err(refused("input readback mismatch; content may have changed"));
        }
        self.verify_focus(&e)
    }
    fn paste(&mut self, _: &str) -> Result<(), AutomationError> {
        Err(AutomationError::Unsupported(
            "clipboard paste is not implemented; clipboard left untouched".into(),
        ))
    }
    fn click(&mut self, selector: &Selector) -> Result<(), AutomationError> {
        if !matches!(selector.role.as_str(), "list_item" | "menu_item") {
            return Err(refused(
                "P1 click only supports SelectionItem; buttons are refused",
            ));
        }
        let e = self.element(selector)?;
        self.usable(&e)?;
        let pattern: IUIAutomationSelectionItemPattern =
            unsafe { e.GetCurrentPatternAs(UIA_SelectionItemPatternId) }.map_err(native_error)?;
        unsafe { pattern.Select() }.map_err(native_error)?;
        if !unsafe { pattern.CurrentIsSelected() }
            .map_err(native_error)?
            .as_bool()
        {
            return Err(refused("selection was not confirmed"));
        }
        self.focused = None;
        self.check_target(true)
    }
    fn expand(&mut self, selector: &Selector) -> Result<(), AutomationError> {
        if !matches!(selector.role.as_str(), "combo_box" | "menu") {
            return Err(refused("expand requires combo_box or menu"));
        }
        let e = self.element(selector)?;
        self.usable(&e)?;
        let pattern: IUIAutomationExpandCollapsePattern =
            unsafe { e.GetCurrentPatternAs(UIA_ExpandCollapsePatternId) }.map_err(native_error)?;
        unsafe { pattern.Expand() }.map_err(native_error)?;
        if unsafe { pattern.CurrentExpandCollapseState() }.map_err(native_error)?
            != ExpandCollapseState_Expanded
        {
            return Err(refused("expansion was not confirmed"));
        }
        self.focused = None;
        self.check_target(true)
    }
    fn assert_(&mut self, assertion: &Assertion) -> Result<(), AutomationError> {
        match assertion {
            Assertion::ElementPresent { selector } => {
                self.element(selector)?;
            }
            Assertion::ElementName { selector, expected } => {
                let e = self.element(selector)?;
                if unsafe { e.CurrentName() }.map_err(native_error)? != *expected {
                    return Err(refused("name assertion failed"));
                }
            }
            Assertion::TextEquals { selector, expected } => {
                let e = self.element(selector)?;
                if unsafe { e.CurrentIsPassword() }
                    .map_err(native_error)?
                    .as_bool()
                {
                    return Err(refused("protected control"));
                }
                if unsafe { Self::value(&e)?.CurrentValue() }.map_err(native_error)? != *expected {
                    return Err(refused("text assertion failed"));
                }
            }
        }
        Ok(())
    }
}
