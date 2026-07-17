//! Browser-automation layer for embedded wry webviews: agent-browser-style
//! snapshots with `@eN` element refs plus JavaScript action builders.
//!
//! Ported from notagent's `browser_automation` module, which follows the
//! architecture proven by the reference implementation's WKWebView port of `vercel-labs/agent-browser`
//! (Apache-2.0): an injected script walks the DOM and returns entries
//! (CSS selector + role + accessible name + depth); the HOST assigns monotonic
//! element refs (`e1`, `e2`, …), keeps the ref→selector table, and renders the
//! aria-snapshot-style outline the agent reads (`- role "name" [ref=eN]`).
//! Actions resolve a ref to its CSS selector and run as self-contained scripts
//! returning a `{ok, error?, value?}` envelope.
//!
//! Refs are session-monotonic: a new snapshot allocates fresh numbers and old
//! refs keep resolving until the page changes underneath them (agent-browser
//! semantics: re-snapshot after navigation).

use std::collections::HashMap;

/// One resolvable element reference from a snapshot.
#[derive(Debug, Clone)]
pub struct RefEntry {
    /// CSS selector captured at snapshot time (resolution target).
    pub selector: String,
    /// Computed ARIA-ish role (explicit `role` attr or implicit from the tag).
    pub role: String,
    /// Accessible name (aria-label, label text, placeholder, trimmed text).
    pub name: String,
}

/// The host-side `@eN` → element table. Numbers are monotonic per webview
/// session and never reused, so stale refs fail loudly instead of silently
/// hitting a different element.
#[derive(Debug, Default)]
pub struct RefMap {
    map: HashMap<String, RefEntry>,
    next_ref: usize,
}

impl RefMap {
    #[must_use]
    pub fn new() -> Self {
        Self { map: HashMap::new(), next_ref: 1 }
    }

    /// Allocates the next `eN` id for `entry` and returns it.
    pub fn allocate(&mut self, entry: RefEntry) -> String {
        let id = format!("e{}", self.next_ref);
        self.next_ref += 1;
        self.map.insert(id.clone(), entry);
        id
    }

    /// Resolves a ref (accepts `e3` and `@e3`) to its entry.
    #[must_use]
    pub fn resolve(&self, ref_id: &str) -> Option<&RefEntry> {
        self.map.get(ref_id.strip_prefix('@').unwrap_or(ref_id))
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.map.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

/// Renders a value as a JavaScript string literal (JSON-escaped), so user
/// input can never break out of a generated script.
#[must_use]
pub fn js_string(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_string())
}

// ── webview evaluation glue ──────────────────────────────────────────────

/// Parses the raw payload an `evaluate_script_with_callback` delivered.
pub fn parse_eval_payload(payload: &str) -> Result<serde_json::Value, String> {
    if payload.is_empty() {
        return Err("script returned no value (syntax error or evaluation blocked)".to_string());
    }
    serde_json::from_str(payload).map_err(|e| format!("unparseable script result: {e}"))
}

/// Evaluates `js` in the webview and resolves the returned channel with the
/// JSON-parsed result. The caller decides the timeout (`select!` against a
/// timer) — the channel resolves to `Canceled` if the webview dies first.
pub fn eval_json(
    webview: &wry::WebView,
    js: &str,
) -> futures::channel::oneshot::Receiver<Result<serde_json::Value, String>> {
    let (tx, rx) = futures::channel::oneshot::channel();
    // wry wants `Fn`, the channel is single-shot — hand it over via a Mutex.
    let tx = std::sync::Mutex::new(Some(tx));
    let queued = webview.evaluate_script_with_callback(js, move |payload| {
        if let Some(tx) = tx.lock().ok().and_then(|mut guard| guard.take()) {
            let _ = tx.send(parse_eval_payload(&payload));
        }
    });
    // On a queue failure the sender is dropped and the receiver resolves to
    // `Canceled`, which callers already treat as an eval failure.
    drop(queued);
    rx
}

// ── snapshot ─────────────────────────────────────────────────────────────

/// One entry the snapshot script collected in the page.
#[derive(Debug, Clone)]
pub struct SnapshotEntry {
    pub selector: String,
    pub role: String,
    pub name: String,
    pub depth: usize,
}

/// The parsed result of one snapshot evaluation.
#[derive(Debug, Clone)]
pub struct SnapshotPage {
    pub title: String,
    pub url: String,
    pub ready_state: String,
    /// Visible page text (fallback content when no entries were found).
    pub text: String,
    pub entries: Vec<SnapshotEntry>,
}

/// Builds the snapshot script. Walks the DOM from `scope` (or `body`),
/// computes role + accessible name per element, filters to interactive (and
/// optionally content) roles, and returns
/// `{title, url, ready_state, text, entries: [{selector, role, name, depth}]}`.
///
/// The role/name/visibility logic mirrors the agent-browser taxonomy
/// (INTERACTIVE_ROLES / CONTENT_ROLES) as ported to DOM JavaScript by the reference implementation;
/// the full-page `outerHTML` of the original is deliberately omitted — the
/// agent tool never consumes it and it dominates payload size.
#[must_use]
pub fn snapshot_script(interactive_only: bool, max_depth: usize, scope: Option<&str>) -> String {
    let interactive = if interactive_only { "true" } else { "false" };
    let scope_literal = scope.map_or_else(|| "null".to_string(), js_string);
    format!(
        r#"(() => {{
  const __interactiveOnly = {interactive};
  const __maxDepth = {max_depth};
  const __scopeSelector = {scope_literal};

  const __normalize = (s) => String(s || '').replace(/\s+/g, ' ').trim();
  const __interactiveRoles = new Set(['button','link','textbox','checkbox','radio','combobox','listbox','menuitem','menuitemcheckbox','menuitemradio','option','searchbox','slider','spinbutton','switch','tab','treeitem']);
  const __contentRoles = new Set(['heading','cell','gridcell','columnheader','rowheader','listitem','article','region','main','navigation']);

  const __isVisible = (el) => {{
    try {{
      if (!el) return false;
      const style = getComputedStyle(el);
      const rect = el.getBoundingClientRect();
      if (!style || !rect) return false;
      if (rect.width <= 0 || rect.height <= 0) return false;
      if (style.display === 'none' || style.visibility === 'hidden') return false;
      if (parseFloat(style.opacity || '1') <= 0.01) return false;
      return true;
    }} catch (_) {{ return false; }}
  }};

  const __implicitRole = (el) => {{
    const tag = String(el.tagName || '').toLowerCase();
    if (tag === 'button') return 'button';
    if (tag === 'a' && el.hasAttribute('href')) return 'link';
    if (tag === 'input') {{
      const type = String(el.getAttribute('type') || 'text').toLowerCase();
      if (type === 'checkbox') return 'checkbox';
      if (type === 'radio') return 'radio';
      if (type === 'submit' || type === 'button' || type === 'reset') return 'button';
      return 'textbox';
    }}
    if (tag === 'textarea') return 'textbox';
    if (tag === 'select') return 'combobox';
    if (tag === 'summary') return 'button';
    if (/^h[1-6]$/.test(tag)) return 'heading';
    if (tag === 'li') return 'listitem';
    if (tag === 'nav') return 'navigation';
    if (tag === 'main') return 'main';
    if (tag === 'article') return 'article';
    return null;
  }};

  const __nameFor = (el) => {{
    const aria = __normalize(el.getAttribute('aria-label') || '');
    if (aria) return aria;
    const labelledBy = __normalize(el.getAttribute('aria-labelledby') || '');
    if (labelledBy) {{
      const text = labelledBy.split(/\s+/).map((id) => document.getElementById(id)).filter(Boolean).map((n) => __normalize(n.textContent || '')).join(' ').trim();
      if (text) return text;
    }}
    const tag = String(el.tagName || '').toLowerCase();
    if (tag === 'input' || tag === 'textarea') {{
      if (el.labels && el.labels.length) {{
        const label = __normalize(el.labels[0].textContent || '');
        if (label) return label;
      }}
      const placeholder = __normalize(el.getAttribute('placeholder') || '');
      if (placeholder) return placeholder;
      const value = __normalize(el.value || '');
      if (value) return value;
    }}
    const title = __normalize(el.getAttribute('title') || '');
    if (title) return title;
    const text = __normalize(el.innerText || el.textContent || '');
    if (text) return text.slice(0, 120);
    return '';
  }};

  const __cssPath = (el) => {{
    if (!el || el.nodeType !== 1) return null;
    if (el.id) return '#' + CSS.escape(el.id);
    const parts = [];
    let cur = el;
    while (cur && cur.nodeType === 1) {{
      let part = String(cur.tagName || '').toLowerCase();
      if (!part) break;
      if (cur.id) {{
        parts.unshift(part + '#' + CSS.escape(cur.id));
        break;
      }}
      const parent = cur.parentElement;
      if (parent) {{
        const siblings = Array.from(parent.children).filter((n) => String(n.tagName || '').toLowerCase() === part);
        if (siblings.length > 1) part += `:nth-of-type(${{siblings.indexOf(cur) + 1}})`;
      }}
      parts.unshift(part);
      cur = cur.parentElement;
      if (parts.length >= 8) break;
    }}
    return parts.join(' > ');
  }};

  const __entries = [];
  const __seen = new Set();
  const __append = (el, depth) => {{
    if (!__isVisible(el)) return;
    const explicitRole = __normalize(el.getAttribute('role') || '').toLowerCase();
    const role = explicitRole || __implicitRole(el) || '';
    if (!role) return;
    if (__interactiveOnly && !__interactiveRoles.has(role)) return;
    if (!__interactiveOnly && !__interactiveRoles.has(role) && !__contentRoles.has(role)) return;
    // Content roles only earn a line when they have a name (agent-browser rule).
    if (!__interactiveRoles.has(role) && !__nameFor(el)) return;
    const selector = __cssPath(el);
    if (!selector || __seen.has(selector)) return;
    __seen.add(selector);
    __entries.push({{ selector, role, name: __nameFor(el), depth }});
  }};

  const __walk = (node, depth) => {{
    if (!node || depth > __maxDepth || node.nodeType !== 1) return;
    __append(node, depth);
    for (const child of Array.from(node.children || [])) __walk(child, depth + 1);
  }};

  const __root = __scopeSelector
    ? (document.querySelector(__scopeSelector) || document.body || document.documentElement)
    : (document.body || document.documentElement);
  if (__root) __walk(__root, 0);

  return {{
    title: __normalize(document.title || ''),
    url: String(location.href || ''),
    ready_state: String(document.readyState || ''),
    text: document.body ? String(document.body.innerText || '').slice(0, 4000) : '',
    entries: __entries
  }};
}})()"#
    )
}

/// Parses the JSON value a snapshot evaluation returned.
pub fn parse_snapshot_result(value: &serde_json::Value) -> Result<SnapshotPage, String> {
    let obj = value.as_object().ok_or("snapshot returned no object")?;
    let str_of = |key: &str| {
        obj.get(key)
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string()
    };
    let entries = obj
        .get("entries")
        .and_then(|v| v.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|e| {
                    let selector = e.get("selector")?.as_str()?.to_string();
                    if selector.is_empty() {
                        return None;
                    }
                    Some(SnapshotEntry {
                        selector,
                        role: e
                            .get("role")
                            .and_then(|v| v.as_str())
                            .filter(|s| !s.is_empty())
                            .unwrap_or("generic")
                            .to_string(),
                        name: e
                            .get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or_default()
                            .trim()
                            .to_string(),
                        depth: e
                            .get("depth")
                            .and_then(serde_json::Value::as_u64)
                            .unwrap_or(0) as usize,
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(SnapshotPage {
        title: str_of("title"),
        url: str_of("url"),
        ready_state: str_of("ready_state"),
        text: str_of("text"),
        entries,
    })
}

/// Renders the outline the agent reads and registers every entry in `refs`.
///
/// Format (agent-browser): a `- document "title"` header, then one
/// `- role "name" [ref=eN]` line per entry, indented two spaces per DOM depth
/// level. Pages without entries fall back to a clipped text excerpt.
#[must_use]
pub fn format_snapshot(page: &SnapshotPage, refs: &mut RefMap) -> String {
    let title = if page.title.is_empty() {
        "page"
    } else {
        &page.title
    };
    let mut lines = vec![format!("- document \"{}\"", title.replace('"', "'"))];

    if page.entries.is_empty() {
        let excerpt: String = page.text.split_whitespace().collect::<Vec<_>>().join(" ");
        if excerpt.is_empty() {
            lines.push("- (empty)".to_string());
        } else {
            let clipped: String = excerpt.chars().take(240).collect();
            lines.push(format!("- text \"{}\"", clipped.replace('"', "'")));
        }
        return lines.join("\n");
    }

    for entry in &page.entries {
        let ref_id = refs.allocate(RefEntry {
            selector: entry.selector.clone(),
            role: entry.role.clone(),
            name: entry.name.clone(),
        });
        let indent = "  ".repeat(entry.depth);
        let mut line = format!("{indent}- {}", entry.role);
        if !entry.name.is_empty() {
            line.push_str(&format!(" \"{}\"", entry.name.replace('"', "'")));
        }
        line.push_str(&format!(" [ref={ref_id}]"));
        lines.push(line);
    }
    lines.join("\n")
}

// ── actions ──────────────────────────────────────────────────────────────

/// Wraps an action body in the standard envelope: resolves `selector` to `el`
/// and returns `{ok:false, error:'not_found', selector}` when it is gone
/// (stale ref / page changed), otherwise runs `body` (which must `return` an
/// object, conventionally `{ok:true, ...}`).
fn with_element(selector: &str, body: &str) -> String {
    let sel = js_string(selector);
    format!(
        r"(() => {{
  const el = document.querySelector({sel});
  if (!el) return {{ ok: false, error: 'not_found', selector: {sel} }};
  try {{
{body}
  }} catch (e) {{ return {{ ok: false, error: String(e && e.message || e) }}; }}
}})()"
    )
}

/// A synthetic, bubbling click (pointer/mouse event sequence + `click()`).
#[must_use]
pub fn click_script(selector: &str) -> String {
    with_element(
        selector,
        r"    el.scrollIntoView({ block: 'center', inline: 'center' });
    const rect = el.getBoundingClientRect();
    const opts = { bubbles: true, cancelable: true, view: window, clientX: rect.left + rect.width / 2, clientY: rect.top + rect.height / 2 };
    el.dispatchEvent(new PointerEvent('pointerdown', opts));
    el.dispatchEvent(new MouseEvent('mousedown', opts));
    el.dispatchEvent(new PointerEvent('pointerup', opts));
    el.dispatchEvent(new MouseEvent('mouseup', opts));
    el.click();
    return { ok: true };",
    )
}

/// A double click (click sequence + `dblclick` event).
#[must_use]
pub fn dblclick_script(selector: &str) -> String {
    with_element(
        selector,
        r"    el.scrollIntoView({ block: 'center', inline: 'center' });
    const opts = { bubbles: true, cancelable: true, view: window };
    el.click();
    el.click();
    el.dispatchEvent(new MouseEvent('dblclick', opts));
    return { ok: true };",
    )
}

/// Sets an input/textarea/select value React-safely (native value setter +
/// `input`/`change` events). Empty `text` clears the field
/// (agent-browser decision: `fill ""` is a clear).
#[must_use]
pub fn fill_script(selector: &str, text: &str) -> String {
    let value = js_string(text);
    with_element(
        selector,
        &format!(
            r"    el.focus();
    const value = {value};
    const proto = el instanceof HTMLTextAreaElement ? HTMLTextAreaElement.prototype
      : el instanceof HTMLSelectElement ? HTMLSelectElement.prototype
      : HTMLInputElement.prototype;
    const setter = Object.getOwnPropertyDescriptor(proto, 'value');
    if (setter && setter.set) setter.set.call(el, value); else el.value = value;
    el.dispatchEvent(new Event('input', {{ bubbles: true }}));
    el.dispatchEvent(new Event('change', {{ bubbles: true }}));
    return {{ ok: true }};"
        ),
    )
}

/// Types `text` by appending to the current value, one `input` event per
/// character (closer to real typing than `fill`).
#[must_use]
pub fn type_script(selector: &str, text: &str) -> String {
    let value = js_string(text);
    with_element(
        selector,
        &format!(
            r"    el.focus();
    const chars = Array.from({value});
    const proto = el instanceof HTMLTextAreaElement ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype;
    const setter = Object.getOwnPropertyDescriptor(proto, 'value');
    for (const ch of chars) {{
      el.dispatchEvent(new KeyboardEvent('keydown', {{ key: ch, bubbles: true }}));
      const next = (el.value || '') + ch;
      if (setter && setter.set) setter.set.call(el, next); else el.value = next;
      el.dispatchEvent(new Event('input', {{ bubbles: true }}));
      el.dispatchEvent(new KeyboardEvent('keyup', {{ key: ch, bubbles: true }}));
    }}
    el.dispatchEvent(new Event('change', {{ bubbles: true }}));
    return {{ ok: true }};"
        ),
    )
}

/// Dispatches a key (`Enter`, `Escape`, `Tab`, single chars …) on the element.
#[must_use]
pub fn press_script(selector: &str, key: &str) -> String {
    let key_lit = js_string(key);
    with_element(
        selector,
        &format!(
            r"    el.focus();
    const key = {key_lit};
    const opts = {{ key, bubbles: true, cancelable: true }};
    const proceed = el.dispatchEvent(new KeyboardEvent('keydown', opts));
    el.dispatchEvent(new KeyboardEvent('keyup', opts));
    if (proceed && key === 'Enter' && el.form && typeof el.form.requestSubmit === 'function') el.form.requestSubmit();
    return {{ ok: true }};"
        ),
    )
}

/// Hovers the element (pointer/mouse over+enter events).
#[must_use]
pub fn hover_script(selector: &str) -> String {
    with_element(
        selector,
        r"    el.scrollIntoView({ block: 'center', inline: 'center' });
    const rect = el.getBoundingClientRect();
    const opts = { bubbles: true, cancelable: true, view: window, clientX: rect.left + rect.width / 2, clientY: rect.top + rect.height / 2 };
    el.dispatchEvent(new PointerEvent('pointerover', opts));
    el.dispatchEvent(new MouseEvent('mouseover', opts));
    el.dispatchEvent(new PointerEvent('pointerenter', opts));
    el.dispatchEvent(new MouseEvent('mouseenter', opts));
    el.dispatchEvent(new MouseEvent('mousemove', opts));
    return { ok: true };",
    )
}

/// Focuses the element.
#[must_use]
pub fn focus_script(selector: &str) -> String {
    with_element(selector, "    el.focus();\n    return { ok: true };")
}

/// Checks/unchecks a checkbox or radio (click only when the state differs, so
/// the page sees a real interaction).
#[must_use]
pub fn set_checked_script(selector: &str, checked: bool) -> String {
    let want = if checked { "true" } else { "false" };
    with_element(
        selector,
        &format!(
            r"    if (!!el.checked !== {want}) el.click();
    return {{ ok: true, checked: !!el.checked }};"
        ),
    )
}

/// Selects option(s) by value in a `<select>` and fires `input`/`change`.
#[must_use]
pub fn select_script(selector: &str, values: &[String]) -> String {
    let wanted = js_string(&serde_json::to_string(values).unwrap_or_else(|_| "[]".into()));
    with_element(
        selector,
        &format!(
            r"    const wanted = new Set(JSON.parse({wanted}));
    let matched = 0;
    for (const option of Array.from(el.options || [])) {{
      const on = wanted.has(option.value) || wanted.has(option.textContent.trim());
      if (el.multiple) {{ option.selected = on; if (on) matched++; }}
      else if (on) {{ el.value = option.value; matched++; break; }}
    }}
    if (!matched) return {{ ok: false, error: 'option_not_found' }};
    el.dispatchEvent(new Event('input', {{ bubbles: true }}));
    el.dispatchEvent(new Event('change', {{ bubbles: true }}));
    return {{ ok: true, selected: matched }};"
        ),
    )
}

/// Scrolls the element into view.
#[must_use]
pub fn scroll_into_view_script(selector: &str) -> String {
    with_element(
        selector,
        "    el.scrollIntoView({ block: 'center', inline: 'center', behavior: 'instant' });\n    return { ok: true };",
    )
}

/// Scrolls the window (or, with a selector, the element) by a pixel delta.
#[must_use]
pub fn scroll_script(selector: Option<&str>, dx: i64, dy: i64) -> String {
    match selector {
        Some(sel) => with_element(
            sel,
            &format!("    el.scrollBy({dx}, {dy});\n    return {{ ok: true }};"),
        ),
        None => format!("(() => {{ window.scrollBy({dx}, {dy}); return {{ ok: true }}; }})()"),
    }
}

/// Getter family: `text|html|value|attr|count|box`. `attr` needs `arg`.
#[must_use]
pub fn get_script(selector: &str, what: &str, arg: Option<&str>) -> String {
    match what {
        "text" => with_element(
            selector,
            "    return { ok: true, value: String(el.innerText || el.textContent || '') };",
        ),
        "html" => with_element(selector, "    return { ok: true, value: el.outerHTML };"),
        "value" => with_element(
            selector,
            "    return { ok: true, value: el.value !== undefined ? String(el.value) : null };",
        ),
        "attr" => {
            let attr = js_string(arg.unwrap_or_default());
            with_element(
                selector,
                &format!("    return {{ ok: true, value: el.getAttribute({attr}) }};"),
            )
        }
        "count" => {
            let sel = js_string(selector);
            format!(
                "(() => {{ return {{ ok: true, value: document.querySelectorAll({sel}).length }}; }})()"
            )
        }
        "box" => with_element(
            selector,
            r"    const r = el.getBoundingClientRect();
    return { ok: true, value: { x: r.x, y: r.y, width: r.width, height: r.height } };",
        ),
        other => format!(
            "(() => {{ return {{ ok: false, error: 'unknown_getter', what: {} }}; }})()",
            js_string(other)
        ),
    }
}

/// State-check family: `visible|enabled|checked`.
#[must_use]
pub fn is_script(selector: &str, what: &str) -> String {
    match what {
        "visible" => {
            let sel = js_string(selector);
            format!(
                r"(() => {{
  const el = document.querySelector({sel});
  if (!el) return {{ ok: true, value: false }};
  const style = getComputedStyle(el);
  const rect = el.getBoundingClientRect();
  const visible = rect.width > 0 && rect.height > 0 && style.display !== 'none' && style.visibility !== 'hidden' && parseFloat(style.opacity || '1') > 0.01;
  return {{ ok: true, value: visible }};
}})()"
            )
        }
        "enabled" => with_element(selector, "    return { ok: true, value: !el.disabled };"),
        "checked" => with_element(selector, "    return { ok: true, value: !!el.checked };"),
        other => format!(
            "(() => {{ return {{ ok: false, error: 'unknown_check', what: {} }}; }})()",
            js_string(other)
        ),
    }
}

/// Condition expression for host-side `wait` polling: true once `selector`
/// exists (and is visible when `visible` is set). The host loops this with a
/// timeout — there is no sleep inside the page.
#[must_use]
pub fn wait_condition_script(selector: &str, visible: bool) -> String {
    let sel = js_string(selector);
    if visible {
        format!(
            r"(() => {{
  const el = document.querySelector({sel});
  if (!el) return false;
  const rect = el.getBoundingClientRect();
  return rect.width > 0 && rect.height > 0;
}})()"
        )
    } else {
        format!("(() => !!document.querySelector({sel}))()")
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn page_with(entries: Vec<SnapshotEntry>) -> SnapshotPage {
        SnapshotPage {
            title: "Example".to_string(),
            url: "https://example.com".to_string(),
            ready_state: "complete".to_string(),
            text: String::new(),
            entries,
        }
    }

    #[test]
    fn refs_are_monotonic_across_snapshots_and_resolve_both_notations() {
        let mut refs = RefMap::new();
        let e1 = refs.allocate(RefEntry {
            selector: "#login".into(),
            role: "button".into(),
            name: "Login".into(),
        });
        assert_eq!(e1, "e1");

        // A second snapshot continues numbering — e1 stays resolvable.
        let e2 = refs.allocate(RefEntry {
            selector: "#signup".into(),
            role: "link".into(),
            name: "Sign up".into(),
        });
        assert_eq!(e2, "e2");
        assert_eq!(refs.resolve("e1").unwrap().selector, "#login");
        assert_eq!(refs.resolve("@e2").unwrap().selector, "#signup");
        assert!(refs.resolve("e3").is_none(), "unknown refs stay unknown");
    }

    #[test]
    fn format_snapshot_renders_the_agent_browser_outline() {
        let mut refs = RefMap::new();
        let page = page_with(vec![
            SnapshotEntry {
                selector: "nav".into(),
                role: "navigation".into(),
                name: "Main".into(),
                depth: 0,
            },
            SnapshotEntry {
                selector: "nav > a".into(),
                role: "link".into(),
                name: "Home".into(),
                depth: 1,
            },
            SnapshotEntry {
                selector: "#cta".into(),
                role: "button".into(),
                name: "Sign \"up\"".into(),
                depth: 0,
            },
        ]);
        let text = format_snapshot(&page, &mut refs);
        assert_eq!(
            text,
            "- document \"Example\"\n\
             - navigation \"Main\" [ref=e1]\n\
             \x20 - link \"Home\" [ref=e2]\n\
             - button \"Sign 'up'\" [ref=e3]",
            "quotes in names are folded to single quotes"
        );
        assert_eq!(refs.resolve("e2").unwrap().selector, "nav > a");
    }

    #[test]
    fn format_snapshot_falls_back_to_a_text_excerpt() {
        let mut refs = RefMap::new();
        let mut page = page_with(Vec::new());
        page.text = "Just   some\nplain text".to_string();
        let text = format_snapshot(&page, &mut refs);
        assert_eq!(
            text,
            "- document \"Example\"\n- text \"Just some plain text\""
        );
        assert!(refs.is_empty(), "no refs without entries");
    }

    #[test]
    fn parse_snapshot_result_skips_malformed_entries() {
        let value = json!({
            "title": "T",
            "url": "u",
            "ready_state": "complete",
            "text": "",
            "entries": [
                { "selector": "#a", "role": "button", "name": "A", "depth": 1 },
                { "selector": "", "role": "button", "name": "empty selector" },
                { "role": "button", "name": "no selector" },
                { "selector": "#b", "role": "", "name": "", "depth": 0 },
            ]
        });
        let page = parse_snapshot_result(&value).unwrap();
        assert_eq!(page.entries.len(), 2);
        assert_eq!(page.entries[0].selector, "#a");
        assert_eq!(page.entries[1].role, "generic", "empty role defaults");
        assert!(parse_snapshot_result(&json!("nope")).is_err());
    }

    #[test]
    fn action_scripts_escape_hostile_input() {
        // A selector/text that would break out of a naive string template.
        let hostile = r#"'"; alert(1); const x = "'"#;
        let escaped = js_string(hostile);
        assert!(
            escaped.contains(r#"\"; alert"#),
            "the breakout quote is escaped: {escaped}"
        );
        for script in [
            click_script(hostile),
            fill_script("#a", hostile),
            type_script("#a", hostile),
            press_script("#a", hostile),
            get_script("#a", "attr", Some(hostile)),
        ] {
            // The hostile input only ever appears in its JSON-escaped form —
            // the raw (unescaped) literal must not survive anywhere.
            assert!(
                script.contains(&escaped),
                "hostile input goes through the js_string seam:\n{script}"
            );
            assert!(
                !script.replace(&escaped, "").contains("alert(1)"),
                "no unescaped copy outside the literal:\n{script}"
            );
        }
        // js_string is the single escaping seam.
        assert_eq!(js_string("a\"b"), r#""a\"b""#);
    }

    #[test]
    fn select_script_embeds_values_as_json() {
        let script = select_script("#s", &["one".to_string(), "two\"x".to_string()]);
        assert!(script.contains("JSON.parse"));
        assert!(
            !script.contains("two\"x\""),
            "raw quote must not survive unescaped"
        );
    }

    #[test]
    fn scroll_and_wait_variants() {
        assert!(scroll_script(None, 0, 400).contains("window.scrollBy(0, 400)"));
        assert!(scroll_script(Some("#list"), 0, -50).contains("el.scrollBy(0, -50)"));
        assert!(wait_condition_script("#done", false).contains("!!document.querySelector"));
        assert!(wait_condition_script("#done", true).contains("getBoundingClientRect"));
    }

    #[test]
    fn snapshot_script_embeds_parameters() {
        let script = snapshot_script(true, 12, Some("#app"));
        assert!(script.contains("const __interactiveOnly = true"));
        assert!(script.contains("const __maxDepth = 12"));
        assert!(script.contains("\"#app\""));
        let script = snapshot_script(false, 6, None);
        assert!(script.contains("const __interactiveOnly = false"));
        assert!(script.contains("const __scopeSelector = null"));
    }

    #[test]
    fn unknown_getter_and_check_fail_closed() {
        assert!(get_script("#a", "cookies", None).contains("unknown_getter"));
        assert!(is_script("#a", "happy").contains("unknown_check"));
    }

    #[test]
    fn parse_eval_payload_variants() {
        assert!(parse_eval_payload("").is_err(), "empty payload is an error");
        assert!(parse_eval_payload("{nope").is_err(), "garbage is an error");
        assert_eq!(parse_eval_payload("{\"ok\":true}").unwrap(), json!({"ok": true}));
        assert_eq!(parse_eval_payload("null").unwrap(), json!(null));
    }
}
