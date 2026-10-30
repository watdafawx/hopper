//! Settings › Servers › "Server files over SFTP or FTP": servers Hopper
//! reaches over the network instead of a shared folder. A list of sites
//! with Test, Edit and Remove, and the add / edit form.

use super::*;

/// The form's text fields.
pub(super) struct SiteForm {
    pub host: Entity<InputState>,
    pub port: Entity<InputState>,
    pub user: Entity<InputState>,
    pub pass: Entity<InputState>,
    pub key: Entity<InputState>,
    pub folder: Entity<InputState>,
}

impl SiteForm {
    pub fn new(window: &mut Window, cx: &mut App) -> Self {
        let field = |hint: &'static str, window: &mut Window, cx: &mut App| cx.new(|cx| InputState::new(window, cx).placeholder(hint));
        SiteForm {
            host: field("files.example.com or 192.168.50.3", window, cx),
            port: field("Usual", window, cx),
            user: field("User name", window, cx),
            pass: cx.new(|cx| InputState::new(window, cx).masked(true).placeholder("Password")),
            key: field("Optional: C:\\Users\\you\\.ssh\\id_ed25519", window, cx),
            folder: field("/home/minecraft/servers", window, cx),
        }
    }

    pub fn inputs(&self) -> [&Entity<InputState>; 6] {
        [&self.host, &self.port, &self.user, &self.pass, &self.key, &self.folder]
    }
}

const PROTOS: [(&str, &str); 3] = [("sftp", "SFTP"), ("ftps", "FTPS (FTP with TLS)"), ("ftp", "FTP (not secure)")];

fn proto_name(p: &str) -> &'static str {
    PROTOS.iter().find(|(k, _)| *k == p).map_or("SFTP", |(_, n)| n)
}

impl SettingsPage {
    pub(super) fn site_rows(&self, s: &hopper::settings::Settings, window: &mut Window, cx: &mut Context<Self>) -> Vec<Row> {
        let mut rows: Vec<Row> = Vec::new();
        let listed: Vec<AnyElement> = s
            .file_sites
            .iter()
            .enumerate()
            .filter(|(_, x)| !(self.site_form_open && self.site_editing.as_deref() == Some(x.id.as_str())))
            .map(|(i, x)| self.site_row(i, x, cx))
            .collect();
        if !listed.is_empty() {
            rows.push(Row::new("files-sites").below(v_flex().gap_1().children(listed).into_any_element()));
        }
        if !self.site_form_open {
            rows.push(Row::new("files-add").control(
                Button::new("files-add")
                    .small()
                    .outline()
                    .icon(IconName::Plus)
                    .label("Add server files")
                    .on_click(cx.listener(|this, _, window, cx| this.open_site_form(None, window, cx))),
            ));
            return rows;
        }
        let theme = cx.theme().clone();
        let rem = window.rem_size();
        let proto = self.site_proto.clone();
        let view = cx.entity().downgrade();
        let picker = widgets::picker(
            "files-proto",
            proto_name(&proto).to_string(),
            PROTOS.iter().map(|(k, n)| (n.to_string(), *k == proto)).collect(),
            rem,
            Rc::new(move |ix, _, cx| {
                if let Some((k, _)) = PROTOS.get(ix) {
                    view.update(cx, |this, cx| {
                        this.site_proto = k.to_string();
                        cx.notify();
                    })
                    .ok();
                }
            }),
        );
        let f = &self.site_form;
        let sftp = proto == "sftp";
        let field = |e: &Entity<InputState>, w: f32| div().w(rems(w)).child(Input::new(e).small());
        rows.push(Row::new("files-proto").control(div().w(rems(15.)).child(picker)));
        rows.push(Row::new("files-host").control(
            h_flex()
                .gap_2()
                .child(field(&f.host, 15.))
                .child(field(&f.port, 5.)),
        ));
        rows.push(Row::new("files-user").control(field(&f.user, 15.)));
        rows.push(
            Row::new("files-pass")
                .control(div().w(rems(15.)).child(Input::new(&f.pass).small().mask_toggle()))
                .note(Some(if self.site_pass_saved {
                    widgets::pill("Saved", theme.success, false).into_any_element()
                } else {
                    widgets::pill("Not saved", theme.muted_foreground, false).into_any_element()
                })),
        );
        if sftp {
            rows.push(Row::new("files-key").control(field(&f.key, 20.)));
        } else if proto == "ftps" {
            rows.push(Row::new("files-cert").control(
                Switch::new("files-self-signed").checked(self.site_ss).on_click(cx.listener(|this, on: &bool, _, cx| {
                    this.site_ss = *on;
                    cx.notify();
                })),
            ));
        } else {
            rows.push(Row::new("files-plain").note(Some(
                status_line(IconName::TriangleAlert, "Plain FTP sends the password and files unencrypted. Use it only on your own network.".into(), theme.warning),
            )));
        }
        rows.push(Row::new("files-folder").control(field(&f.folder, 20.)));
        let testing = self.site_editing.as_ref().is_some_and(|id| self.site_testing.contains(id));
        let result: AnyElement = match self.site_editing.as_ref().and_then(|id| self.site_tests.get(id)) {
            _ if testing => widgets::caption("Connecting…", cx).into_any_element(),
            Some(Ok(t)) => status_line(IconName::CircleCheck, t.clone(), theme.success),
            Some(Err(e)) => status_line(IconName::CircleX, e.clone(), theme.danger),
            None => widgets::caption("Not tested yet.", cx).text_xs().into_any_element(),
        };
        let filled = |e: &Entity<InputState>| !e.read(cx).value().trim().is_empty();
        let can_save = filled(&f.host) && filled(&f.user);
        rows.push(
            Row::new("files-test").note(Some(result)).control(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("files-cancel")
                            .ghost()
                            .small()
                            .label(if self.site_editing.is_none() { "Cancel" } else { "Done" })
                            .on_click(cx.listener(|this, _, window, cx| this.close_site_form(window, cx))),
                    )
                    .child(
                        Button::new("files-save")
                            .small()
                            .primary()
                            .label("Save and test")
                            .loading(testing)
                            .disabled(testing || !can_save)
                            .when(!can_save, |b| b.tooltip("Add the address and user name first"))
                            .on_click(cx.listener(|this, _, window, cx| this.save_site(window, cx))),
                    ),
            ),
        );
        rows
    }

    fn site_row(&self, i: usize, x: &hopper::settings::FileSite, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let store = self.store.read(cx);
        let testing = self.site_testing.contains(&x.id);
        let state = match (self.site_tests.get(&x.id), store.site_error(&x.id)) {
            _ if testing => widgets::pill("Connecting", theme.muted_foreground, false),
            (Some(Err(_)), _) | (None, Some(_)) => widgets::pill("Can't connect", theme.danger, false),
            (Some(Ok(_)), _) => widgets::pill("Connected", theme.success, false),
            (None, None) if store.site_listed(&x.id) => widgets::pill("Connected", theme.success, false),
            (None, None) => widgets::pill("Connecting", theme.muted_foreground, false),
        };
        let detail: Option<AnyElement> = match (self.site_tests.get(&x.id), store.site_error(&x.id)) {
            _ if testing => None,
            (Some(Ok(t)), _) => Some(status_line(IconName::CircleCheck, t.clone(), theme.success)),
            (Some(Err(e)), _) => Some(status_line(IconName::CircleX, e.clone(), theme.danger)),
            (None, Some(e)) => Some(status_line(IconName::CircleX, e.to_string(), theme.danger)),
            _ => None,
        };
        let n = store.servers().iter().filter(|s| store.site_of(&s.root).as_deref() == Some(x.id.as_str())).count();
        let port = if x.port == 0 { hopper::remotefs::default_port(&x.proto) } else { x.port };
        let mut sub = format!("{}://{}@{}:{port}{}", x.proto, x.user, x.host, if x.folder.starts_with('/') { x.folder.clone() } else { format!("/{}", x.folder) });
        match n {
            0 => {}
            1 => sub.push_str(" · 1 server"),
            n => sub.push_str(&format!(" · {n} servers")),
        }
        let asking = self.site_removing.as_deref() == Some(x.id.as_str());
        let (id, id2, id3) = (x.id.clone(), x.id.clone(), x.id.clone());
        v_flex()
            .gap_1()
            .child(
                h_flex()
                    .gap_2()
                    .py_1()
                    .child(Icon::new(IconName::FolderOpen).small().text_color(theme.muted_foreground))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(div().text_sm().font_medium().truncate().child(format!("{} · {}", x.label(), proto_name(&x.proto))))
                            .child(widgets::caption(sub, cx).text_xs().truncate())
                            .when(!x.host_key.is_empty(), |c| {
                                c.child(widgets::caption(format!("Server key {}", x.host_key), cx).text_xs().truncate())
                            }),
                    )
                    .child(state)
                    .child(
                        Button::new(("files-test", i))
                            .outline()
                            .small()
                            .label("Test")
                            .loading(testing)
                            .disabled(testing)
                            .on_click(cx.listener(move |this, _, _, cx| this.test_site(id.clone(), false, cx))),
                    )
                    .child(
                        Button::new(("files-edit", i))
                            .outline()
                            .small()
                            .label("Edit")
                            .on_click(cx.listener(move |this, _, window, cx| this.open_site_form(Some(id2.clone()), window, cx))),
                    )
                    .child(
                        Button::new(("files-remove", i))
                            .ghost()
                            .small()
                            .when(asking, |b| b.danger())
                            .label(if asking { "Remove?" } else { "Remove" })
                            .tooltip("Forget these server files here and delete the saved password. Nothing on the server changes.")
                            .on_click(cx.listener(move |this, _, window, cx| this.remove_site(id3.clone(), window, cx))),
                    ),
            )
            .children(detail)
            .into_any_element()
    }

    pub(super) fn open_site_form(&mut self, id: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let x = id
            .as_ref()
            .and_then(|id| self.store.read(cx).settings().file_sites.iter().find(|x| &x.id == id).cloned())
            .unwrap_or_else(|| hopper::settings::FileSite { proto: "sftp".into(), ..Default::default() });
        self.site_pass_saved = id.as_ref().is_some_and(|id| hopper::remotefs::secret(id).is_some());
        self.site_editing = id;
        self.site_proto = x.proto.clone();
        self.site_ss = x.self_signed;
        self.site_form_open = true;
        self.site_removing = None;
        let port = if x.port == 0 { String::new() } else { x.port.to_string() };
        let values = [x.host.clone(), port, x.user.clone(), String::new(), x.key_file.clone(), x.folder.clone()];
        for (e, v) in self.site_form.inputs().into_iter().zip(values) {
            e.update(cx, |t, cx| t.set_value(v, window, cx));
        }
        cx.notify();
    }

    fn close_site_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for e in self.site_form.inputs() {
            e.update(cx, |t, cx| t.set_value(String::new(), window, cx));
        }
        self.site_form_open = false;
        self.site_editing = None;
        cx.notify();
    }

    fn save_site(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let v = |e: &Entity<InputState>| e.read(cx).value().trim().to_string();
        let f = &self.site_form;
        let (host, port, user, pass, key, folder) = (v(&f.host), v(&f.port), v(&f.user), v(&f.pass), v(&f.key), v(&f.folder));
        let port: u16 = match port.parse() {
            Ok(p) => p,
            Err(_) if port.is_empty() => 0,
            Err(_) => {
                window.push_notification(Notification::error("The port is a number, like 22 or 2022."), cx);
                return;
            }
        };
        // "sftp://host" pasted whole: keep the host.
        let host = host.split("://").last().unwrap_or(&host).trim_end_matches('/').to_string();
        let id = self.site_editing.clone().unwrap_or_else(|| new_panel_id().replacen('p', "f", 1));
        if !pass.is_empty() {
            if let Err(e) = hopper::remotefs::set_secret(&id, &pass) {
                window.push_notification(Notification::error(format!("{e:#}")), cx);
                return;
            }
            self.site_pass_saved = true;
            self.site_form.pass.update(cx, |t, cx| t.set_value(String::new(), window, cx));
        }
        let (proto, ss, id2) = (self.site_proto.clone(), self.site_ss, id.clone());
        self.store.update(cx, |s, cx| {
            s.update_settings(
                move |st| {
                    let x = match st.file_sites.iter_mut().find(|x| x.id == id2) {
                        Some(x) => x,
                        None => {
                            st.file_sites.push(hopper::settings::FileSite { id: id2.clone(), ..Default::default() });
                            st.file_sites.last_mut().expect("just pushed")
                        }
                    };
                    // A different server: learn its key again.
                    if x.host != host || x.port != port || x.proto != proto {
                        x.host_key.clear();
                    }
                    x.proto = proto;
                    x.host = host;
                    x.port = port;
                    x.user = user;
                    x.key_file = key;
                    x.folder = folder;
                    x.self_signed = ss;
                },
                cx,
            )
        });
        self.site_editing = Some(id.clone());
        self.test_site(id, true, cx);
    }

    /// Connects to site `id` and lists its servers; pins its SSH key the
    /// first time. `close`: shut the form when it works.
    fn test_site(&mut self, id: String, close: bool, cx: &mut Context<Self>) {
        let Some(site) = self.store.read(cx).settings().file_sites.iter().find(|x| x.id == id).cloned() else { return };
        self.site_testing.insert(id.clone());
        self.site_tests.remove(&id);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let r = cx.background_spawn(async move { hopper::remotefs::test(&site).map_err(|e| format!("{e:#}")) }).await;
            this.update(cx, |this, cx| {
                this.site_testing.remove(&id);
                let shown = match r {
                    Ok((found, key)) => {
                        if let Some(key) = key {
                            let id2 = id.clone();
                            this.store.update(cx, |s, cx| {
                                s.update_settings(
                                    move |st| {
                                        if let Some(x) = st.file_sites.iter_mut().find(|x| x.id == id2 && x.host_key.is_empty()) {
                                            x.host_key = key;
                                        }
                                    },
                                    cx,
                                )
                            });
                        }
                        let names: Vec<&str> = found.iter().map(|p| p.trim_end_matches('/').rsplit('/').next().unwrap_or(p)).collect();
                        Ok(match found.len() {
                            0 => "Connected, but no server folder (one with server.properties) is in that folder.".to_string(),
                            1 => format!("Connected: 1 server ({})", names.join(", ")),
                            n => format!("Connected: {n} servers ({})", names.join(", ")),
                        })
                    }
                    Err(e) => Err(e),
                };
                if shown.is_ok() {
                    this.store.update(cx, |s, _| s.rediscover_servers());
                    if close && this.site_editing.as_deref() == Some(id.as_str()) {
                        this.site_form_open = false;
                        this.site_editing = None;
                    }
                }
                this.site_tests.insert(id, shown);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn remove_site(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.site_removing.as_deref() != Some(id.as_str()) {
            self.site_removing = Some(id);
            cx.notify();
            return;
        }
        self.site_removing = None;
        if let Err(e) = hopper::remotefs::set_secret(&id, "") {
            window.push_notification(Notification::error(format!("Couldn't delete the password: {e:#}")), cx);
            return;
        }
        self.site_tests.remove(&id);
        self.store.update(cx, |s, cx| {
            s.update_settings(|st| st.file_sites.retain(|x| x.id != id), cx);
            s.rediscover_servers();
        });
        window.push_notification(Notification::info("Removed. Nothing on the server changed."), cx);
        cx.notify();
    }
}
