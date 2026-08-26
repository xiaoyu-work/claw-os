use std::collections::HashMap;
use std::sync::Arc;

use obscura_browser::{BrowserContext, Page};
use obscura_js::ops::InterceptedRequest;
use serde_json::json;

use crate::domains;
use crate::domains::fetch::FetchInterceptState;
use crate::types::{CdpEvent, CdpRequest, CdpResponse};

pub struct CdpContext {
    pub pages: Vec<Page>,
    pub sessions: HashMap<String, String>, // session_id -> page_id
    pub pending_events: Vec<CdpEvent>,
    pub default_context: Arc<BrowserContext>,
    browser_contexts: HashMap<String, Arc<BrowserContext>>,
    page_counter: u32,
    context_counter: u32,
    pub preload_scripts: Vec<(String, String)>, // (identifier, source)
    pub preload_counter: u32,
    // World names registered via Page.createIsolatedWorld. After every
    // navigation Obscura clears execution contexts (via
    // Runtime.executionContextsCleared) and must re-emit a
    // Runtime.executionContextCreated for each registered world, otherwise
    // Playwright/Puppeteer hang waiting for their utility world to come
    // back. Stored as plain Strings (not by-page) — for now we only model
    // a single page in CdpContext anyway.
    pub isolated_worlds: Vec<String>,
    pub fetch_intercept: FetchInterceptState,
    pub intercept_tx: Option<tokio::sync::mpsc::UnboundedSender<InterceptedRequest>>,
}

impl Default for CdpContext {
    fn default() -> Self {
        Self::new()
    }
}

impl CdpContext {
    pub fn new() -> Self {
        Self::new_with_options(None, false)
    }

    pub fn new_with_proxy(proxy: Option<String>) -> Self {
        Self::new_with_options(proxy, false)
    }

    pub fn new_with_options(proxy: Option<String>, stealth: bool) -> Self {
        Self::new_with_full_options(proxy, stealth, None)
    }

    pub fn new_with_full_options(
        proxy: Option<String>,
        stealth: bool,
        user_agent: Option<String>,
    ) -> Self {
        let default_context = Arc::new(BrowserContext::with_full_options(
            "default".to_string(),
            proxy,
            stealth,
            user_agent,
        ));
        CdpContext {
            pages: Vec::new(),
            sessions: HashMap::new(),
            pending_events: Vec::new(),
            default_context,
            browser_contexts: HashMap::new(),
            page_counter: 0,
            context_counter: 0,
            preload_scripts: Vec::new(),
            preload_counter: 0,
            fetch_intercept: FetchInterceptState::new(),
            intercept_tx: None,
            isolated_worlds: Vec::new(),
        }
    }

    pub fn create_page(&mut self) -> String {
        self.create_page_in_context(None)
            .expect("default browser context must exist")
    }

    pub fn create_page_in_context(&mut self, context_id: Option<&str>) -> Result<String, String> {
        let context = self
            .get_browser_context(context_id)
            .ok_or_else(|| format!("Browser context not found: {}", context_id.unwrap_or("")))?;
        self.page_counter += 1;
        let page_id = format!("page-{}", self.page_counter);
        let mut page = Page::new(page_id.clone(), context);
        page.navigate_blank();
        self.pages.push(page);
        Ok(page_id)
    }

    pub fn get_browser_context(&self, context_id: Option<&str>) -> Option<Arc<BrowserContext>> {
        match context_id.filter(|id| !id.is_empty() && *id != self.default_context.id.as_str()) {
            Some(id) => self.browser_contexts.get(id).cloned(),
            None => Some(self.default_context.clone()),
        }
    }

    pub fn browser_context_ids(&self) -> Vec<String> {
        let mut ids = self.browser_contexts.keys().cloned().collect::<Vec<_>>();
        ids.sort();
        ids
    }

    pub fn create_browser_context(&mut self, proxy: Option<String>) -> String {
        self.context_counter += 1;
        let id = format!("context-{}", self.context_counter);
        let context = Arc::new(BrowserContext::with_full_options(
            id.clone(),
            proxy.or_else(|| self.default_context.proxy_url.clone()),
            self.default_context.stealth,
            Some(self.default_context.user_agent.clone()),
        ));
        self.browser_contexts.insert(id.clone(), context);
        id
    }

    pub fn dispose_browser_context(&mut self, context_id: &str) -> Result<Vec<String>, String> {
        if context_id.is_empty() || context_id == self.default_context.id {
            return Err("The default browser context cannot be disposed".to_string());
        }
        if self.browser_contexts.remove(context_id).is_none() {
            return Err(format!("Browser context not found: {context_id}"));
        }

        let removed_pages = self
            .pages
            .iter()
            .filter(|page| page.context.id == context_id)
            .map(|page| page.id.clone())
            .collect::<Vec<_>>();
        self.pages.retain(|page| page.context.id != context_id);
        self.sessions
            .retain(|_, page_id| !removed_pages.contains(page_id));
        Ok(removed_pages)
    }

    pub fn get_page(&self, id: &str) -> Option<&Page> {
        self.pages.iter().find(|p| p.id == id)
    }

    pub fn get_page_mut(&mut self, id: &str) -> Option<&mut Page> {
        self.pages.iter_mut().find(|p| p.id == id)
    }

    pub fn remove_page(&mut self, id: &str) {
        self.pages.retain(|p| p.id != id);
        self.sessions.retain(|_, v| v != id);
    }

    pub fn get_session_page(&self, session_id: &Option<String>) -> Option<&Page> {
        let page_id = session_id.as_ref().and_then(|sid| self.sessions.get(sid))?;
        self.get_page(page_id)
    }

    pub fn get_session_page_mut(&mut self, session_id: &Option<String>) -> Option<&mut Page> {
        let page_id = session_id
            .as_ref()
            .and_then(|sid| self.sessions.get(sid))
            .cloned()?;

        let target_has_js = self.pages.iter().any(|p| p.id == page_id && p.has_js());

        if !target_has_js {
            for page in &mut self.pages {
                if page.id != page_id && page.has_js() {
                    page.suspend_js();
                    break;
                }
            }
            if let Some(target) = self.pages.iter_mut().find(|p| p.id == page_id) {
                target.resume_js();
            }
        }

        self.get_page_mut(&page_id)
    }
}

pub async fn dispatch(req: &CdpRequest, ctx: &mut CdpContext) -> CdpResponse {
    let (domain, method) = match req.method.split_once('.') {
        Some((d, m)) => (d, m),
        None => {
            return CdpResponse::error(
                req.id,
                -32601,
                format!("Invalid method format: {}", req.method),
                req.session_id.clone(),
            );
        }
    };

    let result = match domain {
        "Target" => domains::target::handle(method, &req.params, ctx).await,
        "Browser" => domains::browser::handle(method, &req.params).await,
        "Page" => domains::page::handle(method, &req.params, ctx, &req.session_id).await,
        "DOM" => domains::dom::handle(method, &req.params, ctx, &req.session_id).await,
        "Runtime" => domains::runtime::handle(method, &req.params, ctx, &req.session_id).await,
        "Network" => domains::network::handle(method, &req.params, ctx, &req.session_id).await,
        "Fetch" => domains::fetch::handle(method, &req.params, ctx, &req.session_id).await,
        "Input" => domains::input::handle(method, &req.params, ctx, &req.session_id).await,
        "Storage" => domains::storage::handle(method, &req.params, ctx, &req.session_id).await,
        "LP" => domains::lp::handle(method, &req.params, ctx, &req.session_id).await,
        "Accessibility" => {
            domains::accessibility::handle(method, &req.params, ctx, &req.session_id).await
        }
        // Accepted but no-op. Puppeteer's FrameManager.initialize calls
        // Audits.enable on connect — refusing it breaks puppeteer.connect()
        // before any user code runs.
        "Emulation" | "Log" | "Performance" | "Security" | "CSS" | "ServiceWorker"
        | "Inspector" | "Debugger" | "Profiler" | "HeapProfiler" | "Overlay" | "Audits" => {
            Ok(json!({}))
        }
        _ => Err(format!("Unknown domain: {}", domain)),
    };

    match result {
        Ok(value) => CdpResponse::success(req.id, value, req.session_id.clone()),
        Err(msg) => {
            tracing::warn!("CDP error for {}: {}", req.method, msg);
            CdpResponse::error(req.id, -32601, msg, req.session_id.clone())
        }
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/dispatch.rs"
    ));
}
