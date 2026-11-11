//! First-run sign-in: Sign in with Plex (PIN approved in the browser), pick a
//! server and music library, save the account, relaunch into the app. Or
//! enter a server URL and token by hand.

use std::time::Duration;

use gpui::{Context, Entity, FontWeight, Window, div, prelude::*, px, rgb};
use gpui_component::input::{Input, InputState};

use crate::{account, icons, shell::MONO, theme::Theme};

enum Step {
    Start,
    Waiting { url: String },
    Servers(Vec<account::Server>),
    Working(String),
    Libraries { url: String, token: String, server: String, sections: Vec<(String, String)> },
    Manual,
    Failed(String),
}

pub struct Setup {
    theme: Theme,
    rt: tokio::runtime::Handle,
    http: reqwest::Client,
    step: Step,
    /// Bumped when the user goes back; a running poll sees it and stops.
    run: std::sync::Arc<std::sync::atomic::AtomicU64>,
    url_in: Entity<InputState>,
    token_in: Entity<InputState>,
    /// The server chosen via plex.tv (its id and addresses get saved too).
    found: Option<account::Server>,
}

impl Setup {
    pub fn new(window: &mut Window, cx: &mut Context<Self>, rt: tokio::runtime::Handle) -> Self {
        let (theme, _) = Theme::load(crate::theme::saved_mode());
        Setup {
            theme,
            rt,
            http: reqwest::Client::builder().timeout(Duration::from_secs(20)).build().unwrap_or_default(),
            step: Step::Start,
            run: Default::default(),
            url_in: cx.new(|cx| InputState::new(window, cx).placeholder("http://my-server:32400")),
            token_in: cx.new(|cx| InputState::new(window, cx).placeholder("X-Plex-Token").masked(true)),
            found: None,
        }
    }

    /// Run `fut` on tokio and hand the result back on the UI thread.
    fn call<T: Send + 'static>(
        &self,
        cx: &mut Context<Self>,
        fut: impl std::future::Future<Output = anyhow::Result<T>> + Send + 'static,
        done: impl FnOnce(&mut Self, anyhow::Result<T>, &mut Context<Self>) + 'static,
    ) {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.rt.spawn(async move {
            let _ = tx.send(fut.await);
        });
        cx.spawn(async move |this, cx| {
            if let Ok(res) = rx.await {
                this.update(cx, |this, cx| {
                    done(this, res, cx);
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    fn fail(&mut self, e: anyhow::Error) {
        self.step = Step::Failed(format!("{e:#}"));
    }

    fn sign_in(&mut self, cx: &mut Context<Self>) {
        let run = self.run.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        self.step = Step::Working("Asking Plex for a sign-in code…".into());
        let http = self.http.clone();
        self.call(cx, async move { account::create_pin(&http).await }, move |this, res, cx| match res {
            Ok((id, code)) => {
                let url = account::auth_url(&code);
                let _ = std::process::Command::new("xdg-open").arg(&url).spawn();
                this.step = Step::Waiting { url };
                this.poll(id, code, run, cx);
            }
            Err(e) => this.fail(e),
        });
    }

    /// Check the PIN every 2 s until it's approved, cancelled (the run counter
    /// moves on), or 10 minutes pass.
    fn poll(&mut self, id: u64, code: String, run: u64, cx: &mut Context<Self>) {
        let http = self.http.clone();
        let current = self.run.clone();
        self.call(
            cx,
            async move {
                let deadline = std::time::Instant::now() + Duration::from_secs(600);
                let mut errors = 0;
                while std::time::Instant::now() < deadline {
                    if current.load(std::sync::atomic::Ordering::SeqCst) != run {
                        return Ok(None);
                    }
                    match account::poll_pin(&http, id, &code).await {
                        Ok(Some(t)) => return Ok(Some(t)),
                        Ok(None) => errors = 0,
                        Err(e) if e.is::<account::PinExpired>() => return Ok(None),
                        // Ride out brief network trouble; give up after a run of failures.
                        Err(e) => {
                            errors += 1;
                            if errors >= 5 {
                                return Err(e);
                            }
                        }
                    }
                    tokio::time::sleep(Duration::from_secs(2)).await;
                }
                Ok(None)
            },
            move |this, res, cx| {
                if this.run.load(std::sync::atomic::Ordering::SeqCst) != run {
                    return;
                }
                match res {
                    Ok(Some(token)) => this.load_servers(token, cx),
                    Ok(None) => this.step = Step::Failed("The sign-in code expired. Try again.".into()),
                    Err(e) => this.fail(e),
                }
            },
        );
    }

    fn load_servers(&mut self, account_token: String, cx: &mut Context<Self>) {
        self.step = Step::Working("Finding your Plex servers…".into());
        let http = self.http.clone();
        self.call(cx, async move { account::servers(&http, &account_token).await }, |this, res, cx| match res {
            Ok(s) if s.is_empty() => this.step = Step::Failed("This Plex account can't see any servers.".into()),
            Ok(s) if s.len() == 1 => this.connect(s.into_iter().next().unwrap(), cx),
            Ok(s) => this.step = Step::Servers(s),
            Err(e) => this.fail(e),
        });
    }

    fn connect(&mut self, server: account::Server, cx: &mut Context<Self>) {
        self.step = Step::Working(format!("Connecting to {}…", server.name));
        let http = self.http.clone();
        self.call(
            cx,
            async move {
                let (url, sections) = account::pick_connection(&http, &server).await?;
                Ok((url, server, sections))
            },
            |this, res, cx| match res {
                Ok((url, server, sections)) => {
                    this.found = Some(server.clone());
                    this.libraries(url, server.token, server.name, sections, cx)
                }
                Err(e) => this.fail(e),
            },
        );
    }

    fn libraries(&mut self, url: String, token: String, server: String, sections: Vec<(String, String)>, cx: &mut Context<Self>) {
        match sections.len() {
            0 => self.step = Step::Failed(format!("{server} has no music library.")),
            1 => self.finish(url, token, server, sections[0].0.clone(), cx),
            _ => self.step = Step::Libraries { url, token, server, sections },
        }
    }

    fn manual_continue(&mut self, cx: &mut Context<Self>) {
        let url = self.url_in.read(cx).value().trim().trim_end_matches('/').to_string();
        let token = self.token_in.read(cx).value().trim().to_string();
        if url.is_empty() || token.is_empty() {
            return;
        }
        self.step = Step::Working(format!("Checking {url}…"));
        let http = self.http.clone();
        let (u2, t2) = (url.clone(), token.clone());
        self.call(cx, async move { account::music_sections(&http, &u2, &t2).await }, move |this, res, cx| match res {
            Ok(sections) => this.libraries(url.clone(), token.clone(), url.clone(), sections, cx),
            Err(e) => this.fail(e),
        });
    }

    /// Save the account and start the app proper.
    fn finish(&mut self, url: String, token: String, server: String, section: String, cx: &mut Context<Self>) {
        let mut a = account::Account { url, token, section, server, ..Default::default() };
        if let Some(s) = self.found.as_ref().filter(|s| s.token == a.token) {
            a.remember(s);
        }
        // A different server or library: its caches would show the old one's.
        let changed = account::stored().is_none_or(|old| old.url != a.url || old.section != a.section);
        if let Err(e) = account::save(&a) {
            self.fail(e);
            return;
        }
        if changed {
            account::clear_server_caches();
        }
        match account::relaunch() {
            Ok(()) => cx.quit(),
            Err(e) => self.step = Step::Failed(format!("Signed in, but Crate couldn't restart itself ({e}). Open Crate again.")),
        }
    }

    fn button(id: &'static str, label: &'static str, primary: bool, t: &Theme) -> gpui::Stateful<gpui::Div> {
        div()
            .id(id)
            .h(px(40.0))
            .px(px(18.0))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(20.0))
            .text_size(px(14.0))
            .font_weight(FontWeight::SEMIBOLD)
            .cursor_pointer()
            .when(primary, |d| d.bg(rgb(t.accent)).text_color(rgb(t.base)).hover(|d| d.opacity(0.9)))
            .when(!primary, |d| d.border_1().border_color(rgb(t.line)).text_color(rgb(t.text)).hover(|d| d.bg(rgb(t.raised))))
            .child(label)
    }
}

impl Render for Setup {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = self.theme.clone();
        let text = |s: String| div().text_size(px(14.0)).line_height(px(21.0)).text_color(rgb(t.text2)).child(s);
        let body: gpui::AnyElement = match &self.step {
            Step::Start => div()
                .flex()
                .flex_col()
                .gap(px(14.0))
                .child(text("Sign in with your Plex account to play your music library. Crate opens Plex in your browser to approve it.".into()))
                .child(
                    div()
                        .flex()
                        .gap(px(10.0))
                        .child(Self::button("signin", "Sign in with Plex", true, &t).on_click(cx.listener(|this, _, _, cx| this.sign_in(cx))))
                        .child(Self::button("manual", "Enter server and token", false, &t).on_click(cx.listener(|this, _, _, cx| {
                            this.step = Step::Manual;
                            cx.notify();
                        }))),
                )
                .into_any_element(),
            Step::Waiting { url, .. } => {
                let url2 = url.clone();
                div()
                    .flex()
                    .flex_col()
                    .gap(px(14.0))
                    .child(text("Approve Crate in the Plex page that just opened in your browser (sign in there if it asks).".into()))
                    .child(div().font_family(MONO).text_size(px(12.0)).text_color(rgb(t.muted)).child("Waiting for approval…"))
                    .child(
                        div()
                            .flex()
                            .gap(px(10.0))
                            .child(Self::button("reopen", "Open the page again", false, &t).on_click(cx.listener(move |_, _, _, _| {
                                let _ = std::process::Command::new("xdg-open").arg(&url2).spawn();
                            })))
                            .child(Self::button("cancel", "Cancel", false, &t).on_click(cx.listener(|this, _, _, cx| {
                                this.run.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                                this.step = Step::Start;
                                cx.notify();
                            }))),
                    )
                    .into_any_element()
            }
            Step::Servers(list) => div()
                .flex()
                .flex_col()
                .gap(px(10.0))
                .child(text("Which server has your music?".into()))
                .children(list.iter().cloned().enumerate().map(|(i, s)| {
                    div()
                        .id(("srv", i))
                        .p(px(12.0))
                        .rounded(px(10.0))
                        .border_1()
                        .border_color(rgb(t.border))
                        .bg(rgb(t.surface))
                        .cursor_pointer()
                        .hover(|d| d.bg(rgb(t.raised)))
                        .text_color(rgb(t.text_strong))
                        .child(s.name.clone())
                        .on_click(cx.listener(move |this, _, _, cx| this.connect(s.clone(), cx)))
                }))
                .into_any_element(),
            Step::Libraries { url, token, server, sections } => {
                let (url, token, server) = (url.clone(), token.clone(), server.clone());
                div()
                    .flex()
                    .flex_col()
                    .gap(px(10.0))
                    .child(text(format!("{server} has more than one music library. Which one?")))
                    .children(sections.iter().cloned().enumerate().map(|(i, (key, title))| {
                        let (u, tk, sv) = (url.clone(), token.clone(), server.clone());
                        div()
                            .id(("lib", i))
                            .p(px(12.0))
                            .rounded(px(10.0))
                            .border_1()
                            .border_color(rgb(t.border))
                            .bg(rgb(t.surface))
                            .cursor_pointer()
                            .hover(|d| d.bg(rgb(t.raised)))
                            .text_color(rgb(t.text_strong))
                            .child(title)
                            .on_click(cx.listener(move |this, _, _, cx| this.finish(u.clone(), tk.clone(), sv.clone(), key.clone(), cx)))
                    }))
                    .into_any_element()
            }
            Step::Manual => div()
                .flex()
                .flex_col()
                .gap(px(10.0))
                .child(text("Your server's address and an X-Plex-Token.".into()))
                .child(div().h(px(36.0)).px(px(4.0)).rounded(px(8.0)).border_1().border_color(rgb(t.border)).bg(rgb(t.surface)).child(Input::new(&self.url_in).appearance(false)))
                .child(div().h(px(36.0)).px(px(4.0)).rounded(px(8.0)).border_1().border_color(rgb(t.border)).bg(rgb(t.surface)).child(Input::new(&self.token_in).appearance(false)))
                .child(
                    div()
                        .flex()
                        .gap(px(10.0))
                        .child(Self::button("go", "Continue", true, &t).on_click(cx.listener(|this, _, _, cx| this.manual_continue(cx))))
                        .child(Self::button("back", "Back", false, &t).on_click(cx.listener(|this, _, _, cx| {
                            this.step = Step::Start;
                            cx.notify();
                        }))),
                )
                .into_any_element(),
            Step::Working(msg) => text(msg.clone()).into_any_element(),
            Step::Failed(msg) => div()
                .flex()
                .flex_col()
                .gap(px(14.0))
                .child(div().text_size(px(14.0)).text_color(rgb(t.red)).child(msg.clone()))
                .child(Self::button("retry", "Start over", false, &t).on_click(cx.listener(|this, _, _, cx| {
                    this.run.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    this.step = Step::Start;
                    cx.notify();
                })))
                .into_any_element(),
        };
        div()
            .size_full()
            .bg(rgb(t.base))
            .text_color(rgb(t.text))
            .font_family("Inter")
            .flex()
            .items_center()
            .justify_center()
            .child(
                div()
                    .w(px(520.0))
                    .p(px(32.0))
                    .rounded(px(16.0))
                    .bg(rgb(t.surface))
                    .border_1()
                    .border_color(rgb(t.raised))
                    .flex()
                    .flex_col()
                    .gap(px(22.0))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(12.0))
                            .child(icons::logo(40.0, t.accent, crate::theme::mix(t.base, 0x000000, 0.35), t.line))
                            .child(div().text_size(px(26.0)).font_weight(FontWeight::BOLD).text_color(rgb(t.text_strong)).child("crate")),
                    )
                    .child(body),
            )
    }
}
