//! Embedded browser pane: a Wry webview hosted as a GPUI element (gpui-wry)
//! behind a URL toolbar, rendered as a `LayoutNode::Browser` leaf that drags
//! and splits exactly like terminal and editor panes.
//!
//! The webview is a *native* child view floating above all GPUI content, so
//! visibility must be managed explicitly: the pane recomputes whether its
//! layout slot is actually visible (active tabs, project focus, fullscreen
//! panes, overlay gate) and hides/shows the native view accordingly.
//!
//! The annotation overlay (element picker + comment input, ported from
//! notagent) is injected into every page; captured annotations are collected
//! here — wiring them into chats is a follow-up.

use crate::simple_input::{SimpleInput, SimpleInputState};
use futures::StreamExt;
use futures::channel::mpsc::UnboundedSender;
use gpui::prelude::FluentBuilder;
use gpui::*;
use notmux_files::theme::theme;
use notmux_terminal::TerminalsRegistry;
use notmux_ui::icon_button::icon_button_sized;
use notmux_ui::tokens::ui_text_md;
use notmux_workspace::state::Workspace;
use std::time::Duration;
use wry::WebViewBuilder;
use wry::http::Request;

/// A message sent from the webview's JavaScript back to the pane over IPC.
enum BrowserMsg {
    /// The page navigated; carries the new URL.
    Url(String),
    /// An annotation was created; carries its JSON payload.
    Annotation(String),
    /// The user clicked into the page — focus this pane in the workspace.
    Focused,
}

/// JavaScript injected before every page load. Sets up `window.__annoToggle()`
/// and reports URL changes + annotations over IPC. Ported from notagent's
/// embedded browser.
const ANNOTATION_JS: &str = r#"
(function () {
  if (window.__annoInit) return;
  window.__annoInit = true;

  var mode = false, count = 0, current = null;
  // While a comment input is open, suspend highlighting and new annotations.
  var inputOpen = false;
  // Persistent annotation overlays, anchored to their elements so they follow
  // the page when it reflows (e.g. when the pane is resized).
  var annos = [];

  function report() { try { window.ipc.postMessage('url:' + location.href); } catch (e) {} }
  report();
  window.addEventListener('hashchange', report);

  // Clicking into the page focuses the pane in the host app (capture phase,
  // so pages that stop propagation still report).
  window.addEventListener('mousedown', function () {
    try { window.ipc.postMessage('focus:1'); } catch (e) {}
  }, true);

  // Hover highlight (viewport-fixed).
  var hi = document.createElement('div');
  hi.style.cssText =
    'position:fixed;pointer-events:none;z-index:2147483647;border:2px solid #3b82f6;' +
    'background:rgba(59,130,246,0.18);display:none;';
  document.documentElement.appendChild(hi);

  function highlight(el) {
    var r = el.getBoundingClientRect();
    hi.style.left = r.left + 'px'; hi.style.top = r.top + 'px';
    hi.style.width = r.width + 'px'; hi.style.height = r.height + 'px';
    hi.style.display = 'block';
  }

  // Re-anchor every persistent marker to its element's current position.
  function reposition() {
    for (var i = 0; i < annos.length; i++) {
      var a = annos[i];
      if (!a.el || !a.el.isConnected) continue;
      var r = a.el.getBoundingClientRect();
      a.box.style.left = r.left + 'px'; a.box.style.top = r.top + 'px';
      a.box.style.width = r.width + 'px'; a.box.style.height = r.height + 'px';
      a.marker.style.left = (r.left + r.width - 11) + 'px';
      a.marker.style.top = (r.top - 11) + 'px';
    }
  }
  window.addEventListener('resize', reposition, true);
  window.addEventListener('scroll', reposition, true);

  function selector(el) {
    if (!el || el === document.body) return 'body';
    if (el.id) return '#' + el.id;
    var parts = [], e = el;
    while (e && e.nodeType === 1 && e !== document.body && parts.length < 5) {
      var s = e.tagName.toLowerCase();
      if (typeof e.className === 'string' && e.className.trim()) {
        s += '.' + e.className.trim().split(/\s+/)[0];
      }
      var i = 1, sib = e;
      while ((sib = sib.previousElementSibling)) { if (sib.tagName === e.tagName) i++; }
      parts.unshift(s + ':nth-of-type(' + i + ')');
      e = e.parentElement;
    }
    return parts.join(' > ');
  }

  window.__annoToggle = function () {
    mode = !mode;
    document.documentElement.style.cursor = mode ? 'crosshair' : '';
    if (!mode) {
      hi.style.display = 'none'; current = null;
      // Leaving annotation mode clears all persistent markers from the page.
      for (var i = 0; i < annos.length; i++) {
        if (annos[i].box) annos[i].box.remove();
        if (annos[i].marker) annos[i].marker.remove();
      }
      annos = []; count = 0;
    }
    return mode;
  };

  // Open terminals to route the annotation to, pushed by the host app before
  // annotation mode is enabled: [{id, name}, ...].
  var terminals = [];
  window.__annoSetTerminals = function (list) { terminals = list || []; };

  document.addEventListener('mousemove', function (e) {
    if (!mode || inputOpen) return;
    var el = document.elementFromPoint(e.clientX, e.clientY);
    if (el && el !== hi) { current = el; highlight(el); }
  }, true);

  document.addEventListener('click', function (e) {
    if (!mode || inputOpen) return;
    e.preventDefault(); e.stopPropagation();
    var el = current || document.elementFromPoint(e.clientX, e.clientY);
    if (!el || el === hi) return;
    var r = el.getBoundingClientRect(), sel = selector(el);

    var inputTop = Math.max(0, r.top - 34);
    var input = document.createElement('input');
    input.placeholder = 'Ask about this…';
    input.style.cssText =
      'position:fixed;z-index:2147483647;padding:6px 12px;border-radius:16px;' +
      'border:1px solid #3b82f6;background:#1f2430;color:#fff;font:13px system-ui;' +
      'left:' + r.left + 'px;top:' + inputTop + 'px;';
    document.documentElement.appendChild(input); input.focus();
    // Suspend annotating while this input is open so nothing else gets marked.
    inputOpen = true; hi.style.display = 'none';

    // Terminal picker under the input: the annotation is sent to the selected
    // terminal. Arrow keys move the selection, clicking a row sends directly.
    var menu = null, rows = [], selIdx = 0;
    function renderMenu() {
      rows.forEach(function (row, i) {
        row.style.background = i === selIdx ? '#3b82f6' : 'transparent';
      });
    }
    if (terminals.length) {
      menu = document.createElement('div');
      menu.style.cssText =
        'position:fixed;z-index:2147483647;background:#1f2430;border:1px solid #3b82f6;' +
        'border-radius:8px;overflow:hidden;font:12px system-ui;color:#fff;min-width:200px;' +
        'left:' + r.left + 'px;top:' + (inputTop + 34) + 'px;';
      terminals.forEach(function (t, i) {
        var row = document.createElement('div');
        row.textContent = t.name;
        row.style.cssText = 'padding:5px 10px;cursor:pointer;white-space:nowrap;';
        row.addEventListener('mousedown', function (ev) {
          ev.preventDefault(); ev.stopPropagation();
          selIdx = i; finish();
        });
        menu.appendChild(row); rows.push(row);
      });
      document.documentElement.appendChild(menu);
      renderMenu();
    }

    function cleanup() { input.remove(); if (menu) menu.remove(); inputOpen = false; }
    function cancel() { cleanup(); }
    function finish() {
      var text = input.value;
      var target = terminals[selIdx] ? terminals[selIdx].id : null;
      cleanup();
      count += 1;
      var box = document.createElement('div');
      box.style.cssText =
        'position:fixed;pointer-events:none;z-index:2147483646;border:2px solid #3b82f6;' +
        'background:rgba(59,130,246,0.12);';
      document.documentElement.appendChild(box);
      var marker = document.createElement('div');
      marker.textContent = String(count);
      marker.style.cssText =
        'position:fixed;pointer-events:none;z-index:2147483647;background:#3b82f6;color:#fff;' +
        'border-radius:50%;width:22px;height:22px;display:flex;align-items:center;' +
        'justify-content:center;font:12px system-ui;';
      document.documentElement.appendChild(marker);
      annos.push({ el: el, box: box, marker: marker });
      reposition();
      try {
        window.ipc.postMessage('anno:' + JSON.stringify({
          n: count, selector: sel, input: text, url: location.href,
          elText: ((el.innerText || '').trim()).slice(0, 200),
          terminal_id: target
        }));
      } catch (err) {}
    }
    input.addEventListener('keydown', function (ev) {
      if (ev.key === 'Enter') finish();
      else if (ev.key === 'Escape') cancel();
      else if (ev.key === 'ArrowDown' && rows.length) {
        ev.preventDefault(); selIdx = (selIdx + 1) % rows.length; renderMenu();
      } else if (ev.key === 'ArrowUp' && rows.length) {
        ev.preventDefault(); selIdx = (selIdx + rows.length - 1) % rows.length; renderMenu();
      }
    });
    input.addEventListener('blur', function () { if (input.value.trim()) finish(); else cancel(); });
  }, true);
})();
"#;

/// Builds a child Wry webview for `window`, loading `url`, with the annotation
/// script injected and IPC routed to `tx`. Ported from notagent.
fn build_webview(
    window: &Window,
    url: &str,
    tx: UnboundedSender<BrowserMsg>,
) -> wry::Result<wry::WebView> {
    let webview = WebViewBuilder::new()
        .with_url(url)
        .with_devtools(true)
        .with_initialization_script(ANNOTATION_JS)
        .with_ipc_handler(move |req: Request<String>| {
            let body = req.body();
            if let Some(rest) = body.strip_prefix("url:") {
                let _ = tx.unbounded_send(BrowserMsg::Url(rest.to_string()));
            } else if let Some(rest) = body.strip_prefix("anno:") {
                let _ = tx.unbounded_send(BrowserMsg::Annotation(rest.to_string()));
            } else if body.starts_with("focus:") {
                let _ = tx.unbounded_send(BrowserMsg::Focused);
            }
        })
        .build_as_child(window)?;
    Ok(webview)
}

/// Captures the full visible page of the webview as PNG bytes, delivered
/// asynchronously via `tx`. Sends `None` on failure. Annotation overlays in
/// the page (box + numbered marker) are included in the snapshot. Ported from
/// notagent.
#[cfg(target_os = "macos")]
fn snapshot_page(
    webview: &wry::WebView,
    tx: futures::channel::oneshot::Sender<Option<Vec<u8>>>,
) {
    use std::cell::RefCell;

    use block2::RcBlock;
    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2::{class, msg_send};
    use wry::WebViewExtMacOS;

    unsafe {
        let wk = webview.webview();
        // A configuration without an explicit rect captures the whole view.
        let config: Retained<AnyObject> = msg_send![class!(WKSnapshotConfiguration), new];

        let tx = RefCell::new(Some(tx));
        let handler = RcBlock::new(move |image: *mut AnyObject, _error: *mut AnyObject| {
            let bytes = if image.is_null() {
                None
            } else {
                nsimage_to_png(image)
            };
            if let Some(tx) = tx.borrow_mut().take() {
                let _ = tx.send(bytes);
            }
        });
        let () = msg_send![
            &*wk,
            takeSnapshotWithConfiguration: &*config,
            completionHandler: &*handler,
        ];
    }
}

/// Converts an NSImage (objc pointer) to PNG bytes.
#[cfg(target_os = "macos")]
unsafe fn nsimage_to_png(image: *mut objc2::runtime::AnyObject) -> Option<Vec<u8>> {
    use objc2::runtime::AnyObject;
    use objc2::{class, msg_send};

    let tiff: *mut AnyObject = unsafe { msg_send![image, TIFFRepresentation] };
    if tiff.is_null() {
        return None;
    }
    let rep: *mut AnyObject =
        unsafe { msg_send![class!(NSBitmapImageRep), imageRepWithData: tiff] };
    if rep.is_null() {
        return None;
    }
    // NSBitmapImageFileTypePNG = 4; empty properties dictionary.
    let props: *mut AnyObject = unsafe { msg_send![class!(NSDictionary), dictionary] };
    let png: *mut AnyObject =
        unsafe { msg_send![rep, representationUsingType: 4usize, properties: props] };
    if png.is_null() {
        return None;
    }
    let len: usize = unsafe { msg_send![png, length] };
    let ptr: *const u8 = unsafe { msg_send![png, bytes] };
    if ptr.is_null() || len == 0 {
        return None;
    }
    // SAFETY: `ptr`/`len` come from the NSData `png` which stays alive for
    // the duration of this call; the slice is copied out immediately.
    Some(unsafe { std::slice::from_raw_parts(ptr, len) }.to_vec())
}

#[cfg(not(target_os = "macos"))]
fn snapshot_page(
    _webview: &wry::WebView,
    tx: futures::channel::oneshot::Sender<Option<Vec<u8>>>,
) {
    let _ = tx.send(None);
}

/// Writes page-screenshot PNG bytes to a unique temp file and returns its
/// path. Ported from notagent's `write_temp_png`.
fn write_temp_png(bytes: &[u8], kind: &str) -> Option<std::path::PathBuf> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let id = COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "notmux-{kind}-{}-{}.png",
        std::process::id(),
        id
    ));
    std::fs::write(&path, bytes).ok()?;
    Some(path)
}

/// Turn toolbar input into a loadable URL: bare hosts get `https://`.
/// Empty input stays empty (no page — the webview isn't even created).
pub(crate) fn normalize_url(input: &str) -> String {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    if trimmed.contains("://") || trimmed.starts_with("about:") {
        trimmed.to_string()
    } else {
        format!("https://{}", trimmed)
    }
}

/// One captured page annotation (raw JSON payload from the overlay script).
/// Kept here until the chat wiring lands.
pub struct PageAnnotation {
    pub url: String,
    pub payload: String,
}

pub struct BrowserPane {
    workspace: Entity<Workspace>,
    terminals: TerminalsRegistry,
    project_id: String,
    slot_id: String,
    /// Current URL (kept in sync with navigation, persisted to the layout).
    url: String,
    url_input: Entity<SimpleInputState>,
    webview: Option<Entity<gpui_wry::WebView>>,
    annotation_mode: bool,
    annotations: Vec<PageAnnotation>,
    /// `@eN` element refs handed out by automation `snapshot`s (remote API).
    automation_refs: crate::layout::browser_automation::RefMap,
    focus_handle: FocusHandle,
}

impl BrowserPane {
    pub fn new(
        workspace: Entity<Workspace>,
        terminals: TerminalsRegistry,
        project_id: String,
        slot_id: String,
        url: String,
        cx: &mut Context<Self>,
    ) -> Self {
        let url_input = cx.new(|cx| {
            SimpleInputState::new(cx)
                .placeholder("Enter URL…")
                .default_value(&url)
        });

        // Re-evaluate native-view visibility whenever workspace state changes
        // (tab switches, project focus, fullscreen) …
        cx.observe(&workspace, |this: &mut Self, _, cx| {
            this.sync_webview_visibility(cx);
        })
        .detach();
        // … and poll the overlay gate, which changes without workspace notify.
        cx.spawn(async move |this: WeakEntity<Self>, cx| {
            loop {
                smol::Timer::after(Duration::from_millis(250)).await;
                if this
                    .update(cx, |pane, cx| pane.sync_webview_visibility(cx))
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();

        // Make this pane reachable for remote automation (`notmux browser …`).
        // The entry is weak; a dead one is pruned on the next registry access.
        crate::layout::browser_registry::register(&slot_id, &project_id, cx.weak_entity());

        Self {
            workspace,
            terminals,
            project_id,
            slot_id,
            url,
            url_input,
            webview: None,
            annotation_mode: false,
            annotations: Vec::new(),
            automation_refs: crate::layout::browser_automation::RefMap::new(),
            focus_handle: cx.focus_handle(),
        }
    }

    pub fn slot_id(&self) -> &str {
        &self.slot_id
    }

    /// Current URL (empty while the pane has no page).
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Bring this pane on screen: focus the pane's slot so its project column
    /// scrolls into view. Used by the `open` automation action so it reliably
    /// reveals the page even when reusing an existing pane.
    fn reveal(&mut self, cx: &mut Context<Self>) {
        let (project_id, slot_id) = (self.project_id.clone(), self.slot_id.clone());
        self.workspace.update(cx, |ws, cx| {
            ws.focus_pane_by_slot(&project_id, &slot_id, cx);
        });
    }

    /// Annotations captured so far (chat wiring pending).
    pub fn annotations(&self) -> &[PageAnnotation] {
        &self.annotations
    }

    /// Lazily build the native webview (needs the window). Not created at all
    /// while the pane has no URL — an empty browser is just the themed pane
    /// with the URL bar waiting for input.
    fn ensure_webview(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.webview.is_some() || self.url.is_empty() {
            return;
        }
        let (tx, mut rx) = futures::channel::mpsc::unbounded::<BrowserMsg>();
        let url = self.url.clone();
        match build_webview(window, &url, tx) {
            Ok(webview) => {
                self.webview = Some(cx.new(|cx| gpui_wry::WebView::new(webview, window, cx)));
                cx.spawn(async move |this: WeakEntity<Self>, cx| {
                    while let Some(msg) = rx.next().await {
                        if this
                            .update(cx, |pane, cx| pane.handle_browser_msg(msg, cx))
                            .is_err()
                        {
                            break;
                        }
                    }
                })
                .detach();
            }
            Err(e) => {
                log::error!("failed to create browser webview: {}", e);
            }
        }
    }

    fn handle_browser_msg(&mut self, msg: BrowserMsg, cx: &mut Context<Self>) {
        match msg {
            BrowserMsg::Url(url) => {
                // The blank start page is not a navigation — keep the URL bar
                // empty and don't persist it.
                if url == self.url || url == "about:blank" {
                    return;
                }
                self.url = url.clone();
                self.url_input
                    .update(cx, |input, cx| input.set_value(&url, cx));
                // A navigation re-injects the page script: annotation mode is
                // off again and the injected terminal list is empty.
                self.annotation_mode = false;
                self.push_terminal_list(cx);
                let (project_id, slot_id) = (self.project_id.clone(), self.slot_id.clone());
                self.workspace.update(cx, |ws, cx| {
                    ws.set_browser_url(&project_id, &slot_id, &url, cx);
                });
                cx.notify();
            }
            BrowserMsg::Annotation(payload) => {
                self.annotations.push(PageAnnotation {
                    url: self.url.clone(),
                    payload: payload.clone(),
                });
                self.handle_annotation(payload, cx);
                cx.notify();
            }
            BrowserMsg::Focused => {
                let (project_id, slot_id) = (self.project_id.clone(), self.slot_id.clone());
                self.workspace.update(cx, |ws, cx| {
                    ws.focus_pane_by_slot(&project_id, &slot_id, cx);
                });
            }
        }
    }

    /// Whether this pane's layout slot is actually on screen: reachable via
    /// active tabs, project visible, no other pane fullscreened, no overlay.
    fn should_show_webview(&self, cx: &App) -> bool {
        if notmux_ui::webview_gate::webviews_suspended() {
            return false;
        }
        let ws = self.workspace.read(cx);
        // Modal overlays (settings panel, dialogs) render above all GPUI
        // content but below a native webview — hide it while one is open.
        if ws.focus_manager.is_modal() {
            return false;
        }
        // Another pane fullscreened in this project covers everything.
        if let Some((fs_project, fs_terminal)) = ws.focus_manager.fullscreen_state()
            && fs_project == self.project_id
            && fs_terminal != self.slot_id
        {
            return false;
        }
        // The pane's project must actually be rendered as a column. This is the
        // authoritative filter: `visible_projects` already folds in the
        // focused-project override, the folder filter, and — crucially — the
        // pinned view (where only projects that own a pinned pane appear). A
        // native webview floats above *all* GPUI content, so a browser whose
        // project is off screen would otherwise bleed over whatever view
        // replaced it.
        if !ws.visible_projects().iter().any(|p| p.id == self.project_id) {
            return false;
        }
        let Some(project) = ws.project(&self.project_id) else {
            return false;
        };
        let Some(ref layout) = project.layout else {
            return false;
        };
        let Some(path) = layout.find_browser_path_by_slot(&self.slot_id) else {
            return false;
        };
        // In the pinned view a visible project still renders only its pinned
        // panes — hide the webview when this browser's slot isn't pinned.
        if let Some(pins) = ws.active_pin_filter(&self.project_id)
            && !pins.iter().any(|s| s == &self.slot_id)
        {
            return false;
        }
        layout.is_path_visible(&path)
    }

    fn sync_webview_visibility(&mut self, cx: &mut Context<Self>) {
        let Some(wv) = self.webview.clone() else {
            return;
        };
        let show = self.should_show_webview(cx);
        wv.update(cx, |wv, _| {
            if show && !wv.visible() {
                wv.show();
            } else if !show && wv.visible() {
                wv.hide();
            }
        });
    }

    fn navigate(&mut self, raw: &str, cx: &mut Context<Self>) {
        let url = normalize_url(raw);
        if url.is_empty() {
            return;
        }
        self.url = url.clone();
        self.url_input
            .update(cx, |input, cx| input.set_value(&url, cx));
        if let Some(wv) = &self.webview {
            wv.update(cx, |wv, _| wv.load_url(&url));
        }
        // No webview yet (pane started empty) → the next render builds it
        // with this URL.
        let (project_id, slot_id) = (self.project_id.clone(), self.slot_id.clone());
        self.workspace.update(cx, |ws, cx| {
            ws.set_browser_url(&project_id, &slot_id, &url, cx);
        });
        cx.notify();
    }

    fn eval_js(&self, js: &str, cx: &mut Context<Self>) {
        if let Some(wv) = &self.webview {
            let _ = wv.read(cx).raw().evaluate_script(js);
        }
    }

    fn toggle_annotation_mode(&mut self, cx: &mut Context<Self>) {
        self.annotation_mode = !self.annotation_mode;
        if self.annotation_mode {
            // Fresh terminal list for the picker under the comment input.
            self.push_terminal_list(cx);
        }
        self.eval_js("window.__annoToggle && window.__annoToggle();", cx);
        cx.notify();
    }

    /// Push the project's open terminals (id + display name) into the page so
    /// the annotation input can offer a picker.
    fn push_terminal_list(&mut self, cx: &mut Context<Self>) {
        let list: Vec<serde_json::Value> = {
            let ws = self.workspace.read(cx);
            let Some(project) = ws.project(&self.project_id) else {
                return;
            };
            let Some(ref layout) = project.layout else {
                return;
            };
            let terminals = self.terminals.lock();
            layout
                .collect_terminal_ids()
                .into_iter()
                .filter_map(|tid| {
                    terminals.get(&tid).map(|t| {
                        let name = if let Some(custom) = project.terminal_names.get(&tid) {
                            custom.clone()
                        } else {
                            project.terminal_display_name(&tid, t.title())
                        };
                        serde_json::json!({ "id": tid, "name": name })
                    })
                })
                .collect()
        };
        let js = format!(
            "window.__annoSetTerminals && window.__annoSetTerminals({});",
            serde_json::Value::Array(list)
        );
        self.eval_js(&js, cx);
    }

    /// A finished annotation: screenshot the page (marker visible), then send
    /// three pastes to the chosen terminal — the screenshot path, the element
    /// info, and the user's comment.
    fn handle_annotation(&mut self, json: String, cx: &mut Context<Self>) {
        #[derive(serde::Deserialize)]
        struct Anno {
            #[serde(default)]
            n: u32,
            #[serde(default)]
            selector: String,
            #[serde(default)]
            input: String,
            #[serde(default)]
            url: String,
            #[serde(default, rename = "elText")]
            el_text: String,
            #[serde(default)]
            terminal_id: Option<String>,
        }
        let Ok(anno) = serde_json::from_str::<Anno>(&json) else {
            return;
        };
        let Some(terminal_id) = anno.terminal_id.clone() else {
            log::warn!("browser annotation without target terminal: {}", json);
            return;
        };

        let (tx, rx) = futures::channel::oneshot::channel();
        match &self.webview {
            Some(wv) => snapshot_page(wv.read(cx).raw(), tx),
            None => {
                let _ = tx.send(None);
            }
        }
        cx.spawn(async move |this: WeakEntity<Self>, cx| {
            let bytes = rx.await.ok().flatten();
            let _ = this.update(cx, |pane, cx| {
                pane.deliver_annotation(
                    anno.n,
                    anno.selector,
                    anno.input,
                    anno.url,
                    anno.el_text,
                    terminal_id,
                    bytes,
                    cx,
                );
            });
        })
        .detach();
    }

    /// Sends the three annotation pastes to the target terminal and focuses
    /// it: (1) the screenshot's temp-file path, (2) the element info, (3) the
    /// user's comment.
    #[allow(clippy::too_many_arguments)]
    fn deliver_annotation(
        &mut self,
        n: u32,
        selector: String,
        input: String,
        url: String,
        el_text: String,
        terminal_id: String,
        png: Option<Vec<u8>>,
        cx: &mut Context<Self>,
    ) {
        let Some(terminal) = self.terminals.lock().get(&terminal_id).cloned() else {
            log::warn!("annotation target terminal not found: {}", terminal_id);
            return;
        };

        // 1. Screenshot (temp PNG path; the numbered marker is in the image).
        if let Some(path) = png.as_deref().and_then(|b| write_temp_png(b, "annotation")) {
            terminal.send_paste(&format!("{} ", path.display()));
        }
        // 2. Element info.
        let mut info = format!("[annotation #{} on {} — element: {}", n, url, selector);
        if !el_text.is_empty() {
            info.push_str(&format!(" \"{}\"", el_text));
        }
        info.push_str("] ");
        terminal.send_paste(&info);
        // 3. The user's comment (may be empty — nothing to paste then).
        if !input.trim().is_empty() {
            terminal.send_paste(&format!("{} ", input.trim()));
        }

        // Bring the target terminal into view so the user can review and send.
        let project_id = self.project_id.clone();
        self.workspace.update(cx, |ws, cx| {
            ws.focus_terminal_by_id(&project_id, &terminal_id, cx);
        });
    }

    fn render_toolbar(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let t = theme(cx);
        let input_focused = self
            .url_input
            .read(cx)
            .focus_handle(cx)
            .is_focused(window);
        div()
            .h(px(36.0))
            .px(px(8.0))
            .flex_shrink_0()
            .flex()
            .items_center()
            .gap(px(6.0))
            .bg(rgb(t.bg_header))
            .child(
                icon_button_sized("browser-back", "icons/chevron-left.svg", 24.0, 14.0, &t)
                    .on_click(cx.listener(|this, _, _window, cx| {
                        cx.stop_propagation();
                        this.eval_js("history.back();", cx);
                    })),
            )
            .child(
                icon_button_sized("browser-forward", "icons/chevron-right.svg", 24.0, 14.0, &t)
                    .on_click(cx.listener(|this, _, _window, cx| {
                        cx.stop_propagation();
                        this.eval_js("history.forward();", cx);
                    })),
            )
            .child(
                icon_button_sized("browser-reload", "icons/refresh.svg", 24.0, 13.0, &t)
                    .on_click(cx.listener(|this, _, _window, cx| {
                        cx.stop_propagation();
                        this.eval_js("location.reload();", cx);
                    })),
            )
            .child(if input_focused {
                // Editing: the real input.
                div()
                    .id("browser-url-wrapper")
                    .flex_1()
                    .min_w(px(80.0))
                    .overflow_hidden()
                    .bg(rgb(t.bg_secondary))
                    .border_1()
                    .border_color(rgb(t.border_active))
                    .rounded(px(4.0))
                    .child(SimpleInput::new(&self.url_input).text_size(ui_text_md(cx)))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| {
                        cx.stop_propagation();
                    })
                    .on_key_down(cx.listener(|this, event: &KeyDownEvent, _window, cx| {
                        cx.stop_propagation();
                        if event.keystroke.key.as_str() == "enter" {
                            let raw = this.url_input.read(cx).value().to_string();
                            this.navigate(&raw, cx);
                        }
                    }))
                    .into_any_element()
            } else {
                // Display: the URL as a single truncated line (ellipsis);
                // clicking focuses the input with everything selected.
                let has_url = !self.url.is_empty();
                div()
                    .id("browser-url-display")
                    .flex_1()
                    .min_w(px(80.0))
                    .overflow_hidden()
                    .px(px(8.0))
                    .py(px(3.0))
                    .bg(rgb(t.bg_secondary))
                    .border_1()
                    .border_color(rgb(t.border))
                    .rounded(px(4.0))
                    .cursor_text()
                    .child(
                        div()
                            .w_full()
                            .truncate()
                            .text_size(ui_text_md(cx))
                            .text_color(rgb(if has_url { t.text_primary } else { t.text_muted }))
                            .child(if has_url {
                                self.url.clone()
                            } else {
                                "Enter URL…".to_string()
                            }),
                    )
                    .on_mouse_down(MouseButton::Left, |_, _, cx| {
                        cx.stop_propagation();
                    })
                    .on_click(cx.listener(|this, _, window, cx| {
                        cx.stop_propagation();
                        let handle = this.url_input.read(cx).focus_handle(cx);
                        window.focus(&handle, cx);
                        this.url_input.update(cx, |input, cx| input.select_all(cx));
                        cx.notify();
                    }))
                    .into_any_element()
            })
            .child({
                let active = self.annotation_mode;
                icon_button_sized("browser-annotate", "icons/edit.svg", 24.0, 13.0, &t)
                    .when(active, |d| d.bg(rgb(t.bg_hover)))
                    .on_click(cx.listener(|this, _, _window, cx| {
                        cx.stop_propagation();
                        this.toggle_annotation_mode(cx);
                    }))
            })
    }
}

// ── remote automation (agent-browser command vocabulary) ────────────────────

impl BrowserPane {
    /// Runs one browser-automation action against this pane's webview and
    /// answers through `respond`. Element actions resolve their `@eN` ref via
    /// `automation_refs` (fed by `snapshot`). Only `wait` polls with its own
    /// deadline; everything else resolves with the eval callback. Ported from
    /// notagent's executor (the reference implementation agent-browser port semantics).
    pub(crate) fn automation_execute(
        &mut self,
        req: notmux_core::api::BrowserRequest,
        respond: crate::layout::browser_registry::BrowserRespond,
        cx: &mut Context<Self>,
    ) {
        use crate::layout::browser_automation as auto;

        fn ok_text(text: impl Into<String>) -> Result<serde_json::Value, String> {
            Ok(serde_json::json!({ "text": text.into() }))
        }

        // `open` (re)navigates this pane — it works without a webview (the
        // next render builds one with the new URL).
        if req.action == "open" {
            let Some(url) = req.url.clone().filter(|u| !u.trim().is_empty()) else {
                respond(Err("open requires `url`".to_string()));
                return;
            };
            self.navigate(&url, cx);
            // Reusing an existing pane must still bring it on screen: make its
            // project visible in the overview and focus the pane, so `open`
            // reliably reveals the page (matches add_browser_right for new panes).
            self.reveal(cx);
            respond(ok_text(format!(
                "navigating to {} — use `wait` or `snapshot` once the page is loaded",
                self.url
            )));
            return;
        }

        let Some(wv) = self.webview.clone() else {
            respond(Err(
                "this browser pane has no page yet — `open <url>` first".to_string(),
            ));
            return;
        };

        match req.action.as_str() {
            "back" => {
                self.eval_js("history.back();", cx);
                respond(ok_text("navigated back"));
            }
            "forward" => {
                self.eval_js("history.forward();", cx);
                respond(ok_text("navigated forward"));
            }
            "reload" => {
                self.eval_js("location.reload();", cx);
                respond(ok_text("reloading"));
            }
            "screenshot" => {
                // WKWebView's takeSnapshot fails for hidden views — surface
                // that as a clear error instead of a generic failure.
                if !self.should_show_webview(cx) {
                    respond(Err(
                        "the browser pane is hidden — bring it on screen (focus its \
                         project/tab) to take a screenshot"
                            .to_string(),
                    ));
                    return;
                }
                let (tx, rx) = futures::channel::oneshot::channel();
                snapshot_page(wv.read(cx).raw(), tx);
                cx.spawn(async move |_, _| {
                    respond(match rx.await {
                        Ok(Some(png)) => match write_temp_png(&png, "screenshot") {
                            Some(path) => {
                                let path = path.display().to_string();
                                Ok(serde_json::json!({
                                    "text": path.clone(),
                                    "value": { "path": path },
                                }))
                            }
                            None => Err("failed to write the screenshot file".to_string()),
                        },
                        _ => Err(
                            "screenshot failed (page snapshots are macOS-only)".to_string(),
                        ),
                    });
                })
                .detach();
            }
            "snapshot" => {
                let js = auto::snapshot_script(!req.full.unwrap_or(false), 12, None);
                let rx = auto::eval_json(wv.read(cx).raw(), &js);
                cx.spawn(async move |this: WeakEntity<Self>, cx| {
                    let outcome = match rx.await {
                        Ok(Ok(value)) => match auto::parse_snapshot_result(&value) {
                            Ok(page) => this
                                .update(cx, |this, _| {
                                    let outline =
                                        auto::format_snapshot(&page, &mut this.automation_refs);
                                    serde_json::json!({
                                        "text": format!(
                                            "Page: {}\nURL: {}\n\n{outline}",
                                            page.title, page.url
                                        ),
                                    })
                                })
                                .map_err(|_| "browser pane was closed".to_string()),
                            Err(e) => Err(e),
                        },
                        Ok(Err(e)) => Err(e),
                        Err(_) => Err("webview evaluation was cancelled".to_string()),
                    };
                    respond(outcome);
                })
                .detach();
            }
            "wait" => {
                let Some(selector) = req.selector.clone().filter(|s| !s.is_empty()) else {
                    respond(Err("wait requires `selector`".to_string()));
                    return;
                };
                let timeout_ms = req.timeout_ms.unwrap_or(5_000).min(30_000);
                cx.spawn(async move |this: WeakEntity<Self>, cx| {
                    let deadline =
                        std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
                    loop {
                        let rx = match this.update(cx, |this, cx| {
                            this.webview.clone().map(|wv| {
                                auto::eval_json(
                                    wv.read(cx).raw(),
                                    &auto::wait_condition_script(&selector, true),
                                )
                            })
                        }) {
                            Ok(Some(rx)) => rx,
                            _ => {
                                respond(Err("browser pane was closed".to_string()));
                                return;
                            }
                        };
                        if let Ok(Ok(value)) = rx.await
                            && value.as_bool() == Some(true)
                        {
                            respond(Ok(
                                serde_json::json!({ "text": format!("`{selector}` is visible") }),
                            ));
                            return;
                        }
                        if std::time::Instant::now() >= deadline {
                            respond(Err(format!(
                                "timed out after {timeout_ms}ms waiting for `{selector}`"
                            )));
                            return;
                        }
                        cx.background_executor()
                            .timer(Duration::from_millis(120))
                            .await;
                    }
                })
                .detach();
            }
            "eval" => {
                let Some(js) = req.js.clone().filter(|s| !s.trim().is_empty()) else {
                    respond(Err("eval requires `js`".to_string()));
                    return;
                };
                let rx = auto::eval_json(wv.read(cx).raw(), &js);
                cx.spawn(async move |_, _| {
                    respond(match rx.await {
                        Ok(Ok(value)) => {
                            let text = value.to_string();
                            Ok(serde_json::json!({ "text": text, "value": value }))
                        }
                        Ok(Err(e)) => Err(e),
                        Err(_) => Err("webview evaluation was cancelled".to_string()),
                    });
                })
                .detach();
            }
            // `scroll` works with or without an element target.
            "scroll" => {
                let selector = match req.element.as_deref() {
                    Some(r) => match self.automation_refs.resolve(r) {
                        Some(entry) => Some(entry.selector.clone()),
                        None => {
                            respond(Err(format!(
                                "unknown element ref `{r}` — take a `snapshot` first"
                            )));
                            return;
                        }
                    },
                    None => None,
                };
                let js = auto::scroll_script(
                    selector.as_deref(),
                    req.dx.unwrap_or(0),
                    req.dy.unwrap_or(0),
                );
                Self::respond_with_envelope(
                    auto::eval_json(wv.read(cx).raw(), &js),
                    req.action.clone(),
                    respond,
                    cx,
                );
            }
            // Element-free getters: url from host state, title/count from JS.
            "get" if req.what.as_deref() == Some("url") => {
                respond(Ok(serde_json::json!({ "text": self.url, "value": self.url })));
            }
            "get" if req.what.as_deref() == Some("title") => {
                let rx = auto::eval_json(
                    wv.read(cx).raw(),
                    "(() => ({ ok: true, value: String(document.title || '') }))()",
                );
                Self::respond_with_envelope(rx, req.action.clone(), respond, cx);
            }
            "get" if req.what.as_deref() == Some("count") => {
                let Some(selector) = req.selector.clone().filter(|s| !s.is_empty()) else {
                    respond(Err("get count requires `selector`".to_string()));
                    return;
                };
                let js = auto::get_script(&selector, "count", None);
                Self::respond_with_envelope(
                    auto::eval_json(wv.read(cx).raw(), &js),
                    req.action.clone(),
                    respond,
                    cx,
                );
            }
            // Everything else targets an element ref from the last snapshot.
            action => {
                let Some(entry) = req
                    .element
                    .as_deref()
                    .and_then(|r| self.automation_refs.resolve(r))
                else {
                    respond(Err(format!(
                        "`{action}` requires `element` with a ref from a previous `snapshot` \
                         (e.g. \"e3\"); unknown or missing ref"
                    )));
                    return;
                };
                let selector = entry.selector.clone();
                let js = match action {
                    "click" => auto::click_script(&selector),
                    "dblclick" => auto::dblclick_script(&selector),
                    "hover" => auto::hover_script(&selector),
                    "focus" => auto::focus_script(&selector),
                    "fill" => auto::fill_script(&selector, req.text.as_deref().unwrap_or("")),
                    "type" => auto::type_script(&selector, req.text.as_deref().unwrap_or("")),
                    "press" => {
                        auto::press_script(&selector, req.key.as_deref().unwrap_or("Enter"))
                    }
                    "check" => auto::set_checked_script(&selector, true),
                    "uncheck" => auto::set_checked_script(&selector, false),
                    "select" => {
                        auto::select_script(&selector, &req.values.clone().unwrap_or_default())
                    }
                    "scroll_into_view" => auto::scroll_into_view_script(&selector),
                    "get" => auto::get_script(
                        &selector,
                        req.what.as_deref().unwrap_or("text"),
                        req.attr.as_deref(),
                    ),
                    "is" => auto::is_script(&selector, req.what.as_deref().unwrap_or("visible")),
                    unknown => {
                        respond(Err(format!("unknown browser action `{unknown}`")));
                        return;
                    }
                };
                Self::respond_with_envelope(
                    auto::eval_json(wv.read(cx).raw(), &js),
                    action.to_string(),
                    respond,
                    cx,
                );
            }
        }
    }

    /// Awaits one action-script evaluation and translates its
    /// `{ok, error?, value?}` envelope into the response payload.
    fn respond_with_envelope(
        rx: futures::channel::oneshot::Receiver<Result<serde_json::Value, String>>,
        action: String,
        respond: crate::layout::browser_registry::BrowserRespond,
        cx: &mut Context<Self>,
    ) {
        cx.spawn(async move |_, _| {
            let outcome = match rx.await {
                Ok(Ok(value)) => {
                    if value.get("ok").and_then(serde_json::Value::as_bool) == Some(true) {
                        let text = match value.get("value") {
                            Some(serde_json::Value::String(s)) => s.clone(),
                            Some(v) if !v.is_null() => v.to_string(),
                            _ => format!("{action}: done"),
                        };
                        let payload =
                            value.get("value").cloned().unwrap_or(serde_json::Value::Null);
                        Ok(serde_json::json!({ "text": text, "value": payload }))
                    } else {
                        let error = value
                            .get("error")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("action failed");
                        Err(if error == "not_found" {
                            "element not found — the page may have changed; take a fresh `snapshot`"
                                .to_string()
                        } else {
                            error.to_string()
                        })
                    }
                }
                Ok(Err(e)) => Err(e),
                Err(_) => Err("webview evaluation was cancelled".to_string()),
            };
            respond(outcome);
        })
        .detach();
    }
}

impl Focusable for BrowserPane {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for BrowserPane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx);
        self.ensure_webview(window, cx);
        self.sync_webview_visibility(cx);
        let toolbar = self.render_toolbar(window, cx);

        div()
            .size_full()
            .min_h_0()
            .min_w_0()
            .flex()
            .flex_col()
            .bg(rgb(t.bg_primary))
            .track_focus(&self.focus_handle)
            .child(toolbar)
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .relative()
                    .overflow_hidden()
                    .when_some(self.webview.clone(), |d, wv| d.child(wv)),
            )
    }
}
