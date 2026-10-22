//! An instance's Peers tab: other PCs running Hopper (LAN, Tailscale or by
//! address), pairing with a code, and pulling or pushing the instance's
//! files with a paired peer.

use crate::store::Store;
use crate::widgets;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::tag::Tag;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, Sizable as _, StyledExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use hopper::peer::{self, DiffEntry, Hub, SharedInst, Status};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

pub struct PeersView {
    store: Entity<Store>,
    root: Option<PathBuf>,
    address: Entity<InputState>,
    message: Option<Result<String, String>>,
    /// Codes to compare after pairing, by peer.
    codes: HashMap<String, String>,
    selected: Option<String>,
    remote: Option<Result<Vec<SharedInst>, String>>,
    remote_inst: String,
    folder: &'static str,
    diff: Option<Result<Vec<DiffEntry>, String>>,
    picked: HashSet<String>,
    running: bool,
    _subscriptions: Vec<Subscription>,
}

impl PeersView {
    pub fn new(store: Entity<Store>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let address = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Add by address: 100.x.y.z, host:port or https URL")
        });
        let subs = vec![
            cx.observe(&store, |_, _, cx| cx.notify()),
            cx.subscribe(&address, |this, _, e: &InputEvent, cx| {
                if matches!(e, InputEvent::PressEnter { .. }) {
                    this.add_address(cx);
                }
            }),
        ];
        Self {
            store,
            root: None,
            address,
            message: None,
            codes: HashMap::new(),
            selected: None,
            remote: None,
            remote_inst: String::new(),
            folder: "mods",
            diff: None,
            picked: HashSet::new(),
            running: false,
            _subscriptions: subs,
        }
    }

    pub fn show_for(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        if self.root.as_ref() != Some(&root) {
            self.diff = None;
            self.picked.clear();
            self.remote_inst.clear();
        }
        self.root = Some(root);
        cx.notify();
    }

    fn hub(&self, cx: &App) -> Option<Arc<Hub>> {
        self.store.read(cx).peer_hub()
    }

    fn add_address(&mut self, cx: &mut Context<Self>) {
        let addr = self.address.read(cx).value().trim().to_string();
        let Some(hub) = self.hub(cx) else { return };
        if addr.is_empty() {
            return;
        }
        cx.spawn(async move |this, cx| {
            let a = addr.clone();
            let r = cx
                .background_spawn(async move { hub.add_address(&a).map(|_| ()) })
                .await;
            this.update(cx, |this, cx| {
                this.message = Some(
                    r.map(|_| "Found it. Pair to sync.".to_string())
                        .map_err(|e| format!("{addr}: {e:#}")),
                );
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn pair(&mut self, id: String, cx: &mut Context<Self>) {
        let Some(hub) = self.hub(cx) else { return };
        cx.spawn(async move |this, cx| {
            let i = id.clone();
            let r = cx.background_spawn(async move { hub.pair(&i) }).await;
            this.update(cx, |this, cx| {
                match r {
                    Ok(code) => {
                        this.codes.insert(id, code);
                    }
                    Err(e) => this.message = Some(Err(format!("Pairing failed: {e:#}"))),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn select(&mut self, id: String, cx: &mut Context<Self>) {
        self.selected = Some(id.clone());
        self.remote = None;
        self.diff = None;
        let Some(hub) = self.hub(cx) else { return };
        cx.spawn(async move |this, cx| {
            let r = cx
                .background_spawn(async move { hub.instances(&id) })
                .await
                .map_err(|e| format!("{e:#}"));
            this.update(cx, |this, cx| {
                this.remote = Some(r);
                this.pick_remote_inst(cx);
                this.compare(cx);
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// Their instance to compare with: the same pack, else the same name.
    fn pick_remote_inst(&mut self, cx: &App) {
        let (Some(Ok(list)), Some(me)) = (&self.remote, self.me(cx)) else {
            return;
        };
        if list.iter().any(|r| r.key == self.remote_inst) {
            return;
        }
        self.remote_inst = list
            .iter()
            .find(|r| me.pack_id != 0 && r.pack_id == me.pack_id)
            .or_else(|| list.iter().find(|r| r.name == me.name))
            .or(list.first())
            .map(|r| r.key.clone())
            .unwrap_or_default();
    }

    fn me(&self, cx: &App) -> Option<SharedInst> {
        let root = self.root.as_ref()?;
        self.store.read(cx).instance(root).map(SharedInst::of)
    }

    fn compare(&mut self, cx: &mut Context<Self>) {
        let (Some(hub), Some(id), Some(me)) = (self.hub(cx), self.selected.clone(), self.me(cx))
        else {
            return;
        };
        if self.remote_inst.is_empty() {
            return;
        }
        let names: HashMap<String, String> = self
            .root
            .as_ref()
            .and_then(|r| self.store.read(cx).instance(r))
            .map(|i| {
                i.addons
                    .iter()
                    .map(|a| (a.file_name.to_lowercase(), a.name.clone()))
                    .collect()
            })
            .unwrap_or_default();
        self.running = true;
        self.picked.clear();
        let (rinst, folder) = (self.remote_inst.clone(), self.folder);
        cx.spawn(async move |this, cx| {
            let r = cx
                .background_spawn(async move {
                    let theirs = hub.remote_manifest(&id, &rinst, folder)?;
                    let mine = peer::manifest(&me, folder, &names);
                    anyhow::Ok(peer::diff(&mine, &theirs))
                })
                .await
                .map_err(|e| format!("{e:#}"));
            this.update(cx, |this, cx| {
                this.running = false;
                this.diff = Some(r);
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn transfer(&mut self, pull: bool, files: Vec<String>, cx: &mut Context<Self>) {
        let (Some(hub), Some(id), Some(root)) = (self.hub(cx), self.selected.clone(), self.root.clone())
        else {
            return;
        };
        let peer_name = hub
            .peers()
            .into_iter()
            .find(|p| p.peer_id == id)
            .map(|p| p.name)
            .unwrap_or_default();
        self.running = true;
        self.message = None;
        let (rinst, folder) = (self.remote_inst.clone(), self.folder);
        cx.spawn(async move |this, cx| {
            let r = cx
                .background_spawn(async move {
                    if pull {
                        let results = hub.pull(&id, &rinst, folder, &root, &files);
                        hopper::history::record(&root, &format!("from {peer_name}"), &root, &results);
                        let failed: Vec<String> =
                            results.iter().filter_map(|r| r.error.clone()).collect();
                        if failed.is_empty() {
                            Ok(format!(
                                "Pulled {} files. Replaced ones are in .hopper-backups.",
                                results.len()
                            ))
                        } else {
                            Err(format!("{} failed: {}", failed.len(), failed[0]))
                        }
                    } else {
                        let failed = hub.push(&id, &rinst, folder, &root, &files);
                        if failed.is_empty() {
                            Ok(format!("Sent {} files.", files.len()))
                        } else {
                            Err(format!("{} failed: {}", failed.len(), failed[0]))
                        }
                    }
                })
                .await;
            this.update(cx, |this, cx| {
                this.running = false;
                this.message = Some(r);
                this.compare(cx);
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn intro(&self, cx: &mut Context<Self>) -> AnyElement {
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap_3()
            .px_6()
            .child(Icon::new(IconName::Users).large().text_color(cx.theme().muted_foreground))
            .child(
                div()
                    .text_lg()
                    .font_semibold()
                    .child("Sync this instance with a friend's or your other PC, directly"),
            )
            .child(
                div().max_w(rems(36.)).text_sm().text_center().child(
                    "Hopper finds other PCs running Hopper on your network or Tailscale. Pair once with a 6-digit code, then compare and pull or push mods, configs and packs.",
                ),
            )
            .child(widgets::caption(
                "Windows may ask whether to allow Hopper on private networks; allow it so the other PC can reach this one.",
                cx,
            ).text_xs())
            .child(
                Button::new("peers-on")
                    .primary()
                    .icon(IconName::Users)
                    .label("Turn on peer sync")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.store.update(cx, |s, cx| s.set_peer_sync(true, cx))
                    })),
            )
            .when_some(self.store.read(cx).peer_error(), |c, e| {
                c.child(div().text_sm().text_color(cx.theme().danger).child(e))
            })
            .into_any_element()
    }

    fn peer_list(&self, hub: &Arc<Hub>, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let peers = hub.peers();
        let empty = peers.is_empty();
        v_flex()
            .gap_1()
            .children(peers.into_iter().map(|p| {
                let online = p.last_seen > 0;
                let active = self.selected.as_deref() == Some(p.peer_id.as_str());
                let (id1, id2, id3) = (p.peer_id.clone(), p.peer_id.clone(), p.peer_id.clone());
                h_flex()
                    .gap_3()
                    .px_3()
                    .py_2()
                    .rounded(theme.radius)
                    .when(active, |r| r.bg(theme.list_active))
                    .child(
                        div()
                            .size_2()
                            .rounded_full()
                            .bg(if online { theme.success } else { theme.muted_foreground }),
                    )
                    .child(div().text_sm().font_semibold().child(p.name.clone()))
                    .when(!p.source.is_empty(), |r| {
                        r.child(Tag::secondary().xsmall().child(p.source.clone()))
                    })
                    .child(widgets::caption(p.addr.clone(), cx).text_xs())
                    .child(div().flex_1())
                    .when_some(self.codes.get(&p.peer_id).cloned(), |r, c| {
                        r.child(div().text_sm().child(format!("Code on their screen should be {c}")))
                    })
                    .map(|r| {
                        if p.trusted {
                            r.child(Tag::success().xsmall().child("Paired"))
                                .child(
                                    Button::new(SharedString::from(format!("sync:{id1}")))
                                        .small()
                                        .map(|b| if active { b.primary() } else { b.outline() })
                                        .label("Sync with")
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.select(id1.clone(), cx)
                                        })),
                                )
                                .child(
                                    Button::new(SharedString::from(format!("forget:{id2}")))
                                        .small()
                                        .ghost()
                                        .label("Forget")
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            peer::forget(&id2);
                                            if this.selected.as_deref() == Some(id2.as_str()) {
                                                this.selected = None;
                                            }
                                            cx.notify();
                                        })),
                                )
                        } else {
                            r.child(
                                Button::new(SharedString::from(format!("pair:{id3}")))
                                    .small()
                                    .primary()
                                    .label("Pair")
                                    .disabled(!online)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.pair(id3.clone(), cx)
                                    })),
                            )
                        }
                    })
            }))
            .when(empty, |c| {
                c.child(widgets::caption(
                    "No other Hopper found yet. Turn on peer sync on the other PC too, or add it by address.",
                    cx,
                ))
            })
    }

    fn sync_panel(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        self.selected.as_ref()?;
        let theme = cx.theme();
        let list = match &self.remote {
            None => {
                return Some(
                    h_flex()
                        .gap_2()
                        .child(Spinner::new().small())
                        .child(widgets::caption("Asking the other PC for its instances", cx))
                        .into_any_element(),
                );
            }
            Some(Err(e)) => {
                return Some(div().text_sm().text_color(theme.danger).child(e.clone()).into_any_element());
            }
            Some(Ok(l)) => l.clone(),
        };
        let rem = theme.font_size;
        let me = self.me(cx)?;
        let items: Vec<(String, bool)> = list
            .iter()
            .map(|r| {
                let label = if r.pack_version.is_empty() {
                    r.name.clone()
                } else {
                    format!("{} ({})", r.name, r.pack_version)
                };
                (label, r.key == self.remote_inst)
            })
            .collect();
        let keys: Vec<String> = list.iter().map(|r| r.key.clone()).collect();
        let view = cx.entity();
        let pick = std::rc::Rc::new(move |ix: usize, _: &mut Window, cx: &mut App| {
            if let Some(k) = keys.get(ix).cloned() {
                view.update(cx, |this, cx| {
                    this.remote_inst = k;
                    this.compare(cx);
                });
            }
        });
        let current = list
            .iter()
            .find(|r| r.key == self.remote_inst)
            .map_or("Pick one".to_string(), |r| r.name.clone());
        let folder_ix = peer::FOLDERS.iter().position(|f| *f == self.folder).unwrap_or(0);

        let diff: AnyElement = match &self.diff {
            None => h_flex()
                .gap_2()
                .child(Spinner::new().small())
                .child(widgets::caption("Comparing", cx))
                .into_any_element(),
            Some(Err(e)) => div().text_sm().text_color(theme.danger).child(e.clone()).into_any_element(),
            Some(Ok(entries)) => {
                let changed: Vec<DiffEntry> =
                    entries.iter().filter(|e| e.status != Status::Same).cloned().collect();
                let same = entries.len() - changed.len();
                let pull: Vec<String> = changed
                    .iter()
                    .filter(|e| self.picked.contains(&e.rel) && e.status != Status::OnlyLocal)
                    .map(|e| e.rel.clone())
                    .collect();
                let push: Vec<String> = changed
                    .iter()
                    .filter(|e| self.picked.contains(&e.rel) && e.status != Status::OnlyRemote)
                    .map(|e| e.rel.clone())
                    .collect();
                let theirs_only: Vec<String> = changed
                    .iter()
                    .filter(|e| e.status == Status::OnlyRemote)
                    .map(|e| e.rel.clone())
                    .collect();
                let differ: Vec<String> = changed
                    .iter()
                    .filter(|e| e.status == Status::Differ)
                    .map(|e| e.rel.clone())
                    .collect();
                let (np, nu) = (pull.len(), push.len());
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .gap_3()
                    .child(
                        h_flex()
                            .gap_2()
                            .flex_wrap()
                            .child(widgets::caption(format!("{} differ · {same} identical", changed.len()), cx))
                            .child(
                                Button::new("sel-theirs")
                                    .xsmall()
                                    .ghost()
                                    .label("Select what they have that I don't")
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.picked.extend(theirs_only.iter().cloned());
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new("sel-changed")
                                    .xsmall()
                                    .ghost()
                                    .label("Select changed")
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.picked.extend(differ.iter().cloned());
                                        cx.notify();
                                    })),
                            )
                            .child(div().flex_1())
                            .child(
                                Button::new("push-peer")
                                    .small()
                                    .outline()
                                    .icon(IconName::Upload)
                                    .label(format!("Push to them ({nu})"))
                                    .tooltip("Send these files to their PC; they keep backups of what's replaced")
                                    .disabled(nu == 0 || self.running)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.transfer(false, push.clone(), cx)
                                    })),
                            )
                            .child(
                                Button::new("pull-peer")
                                    .small()
                                    .primary()
                                    .icon(IconName::Download)
                                    .label(format!("Pull to this PC ({np})"))
                                    .tooltip("Copy their files here; what's replaced is backed up")
                                    .disabled(np == 0 || self.running)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.transfer(true, pull.clone(), cx)
                                    })),
                            ),
                    )
                    .child(
                        v_flex()
                            .id("peer-diff")
                            .flex_1()
                            .min_h_0()
                            .children(changed.into_iter().map(|e| {
                                let picked = self.picked.contains(&e.rel);
                                let rel = e.rel.clone();
                                let tag = match e.status {
                                    Status::Differ => Tag::info().xsmall().child("Different"),
                                    Status::OnlyLocal => Tag::warning().xsmall().child("Only here"),
                                    Status::OnlyRemote => Tag::primary().xsmall().child("Only theirs"),
                                    Status::Same => Tag::secondary().xsmall().child("Same"),
                                };
                                let name = e
                                    .local
                                    .as_ref()
                                    .or(e.remote.as_ref())
                                    .map(|f| f.name.clone())
                                    .filter(|n| !n.is_empty());
                                h_flex()
                                    .gap_3()
                                    .py_1()
                                    .child(
                                        Checkbox::new(SharedString::from(format!("pp:{rel}")))
                                            .checked(picked)
                                            .on_change(cx.listener(move |this, v: &bool, _, cx| {
                                                if *v {
                                                    this.picked.insert(rel.clone());
                                                } else {
                                                    this.picked.remove(&rel);
                                                }
                                                cx.notify();
                                            })),
                                    )
                                    .child(div().w_24().flex_none().child(tag))
                                    .map(|r| match name {
                                        Some(n) => {
                                            // This instance's copy of the mod: icon and page.
                                            let file = e.rel.rsplit('/').next().unwrap_or(&e.rel);
                                            let addon = self
                                                .root
                                                .as_ref()
                                                .and_then(|root| self.store.read(cx).instance(root))
                                                .and_then(|i| i.addon_by_file(file).cloned());
                                            let pid = addon.as_ref().map_or(0, |a| a.project_id);
                                            let icon = addon
                                                .as_ref()
                                                .and_then(|a| self.store.read(cx).image(&a.icon))
                                                .map(std::path::Path::to_path_buf);
                                            let (store, root) = (self.store.clone(), self.root.clone());
                                            r.child(
                                                widgets::mod_link(
                                                    SharedString::from(format!("pmod:{}", e.rel)),
                                                    n,
                                                    icon.as_deref(),
                                                    pid,
                                                    move |project, _, cx| {
                                                        if let Some(root) = root.clone() {
                                                            store.update(cx, |_, cx| {
                                                                cx.emit(crate::store::StoreEvent::Peek { root, project })
                                                            });
                                                        }
                                                    },
                                                    cx,
                                                )
                                                .text_sm()
                                                .font_medium(),
                                            )
                                            .child(widgets::caption(e.rel.clone(), cx).text_xs())
                                        }
                                        None => r.child(div().text_sm().child(e.rel.clone())),
                                    })
                            }))
                            .overflow_y_scrollbar(),
                    )
                    .into_any_element()
            }
        };
        Some(
            v_flex()
                .flex_1()
                .min_h_0()
                .gap_3()
                .pt_3()
                .border_t_1()
                .border_color(theme.border)
                .child(
                    h_flex()
                        .gap_3()
                        .child(div().text_sm().font_semibold().child(format!("{} here", me.name)))
                        .child(Icon::new(IconName::ArrowLeftRight).small())
                        .child(div().w_72().child(widgets::picker("peer-inst", current, items, rem, pick)))
                        .child(
                            TabBar::new("peer-folder")
                                .segmented()
                                .small()
                                .selected_index(folder_ix)
                                .children(peer::FOLDERS.iter().map(|f| Tab::new().label(format!("{f}/"))))
                                .on_click(cx.listener(|this, ix: &usize, _, cx| {
                                    this.folder = peer::FOLDERS[*ix];
                                    this.compare(cx);
                                })),
                        )
                        .child(
                            Button::new("peer-compare")
                                .small()
                                .ghost()
                                .icon(IconName::RefreshCw)
                                .tooltip("Compare again")
                                .disabled(self.running)
                                .on_click(cx.listener(|this, _, _, cx| this.compare(cx))),
                        ),
                )
                .child(diff)
                .into_any_element(),
        )
    }
}

impl Render for PeersView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(hub) = self.hub(cx) else {
            return self.intro(cx);
        };
        let theme = cx.theme();
        let message: Option<AnyElement> = self.message.as_ref().map(|m| match m {
            Ok(t) => div().text_sm().text_color(theme.success).child(t.clone()).into_any_element(),
            Err(t) => div().text_sm().text_color(theme.danger).child(t.clone()).into_any_element(),
        });
        let discovery = hub.discovery_error.clone();
        let peers = self.peer_list(&hub, cx);
        let panel = self.sync_panel(cx);
        v_flex()
            .size_full()
            .px_6()
            .pb_6()
            .gap_4()
            .child(
                h_flex()
                    .gap_3()
                    .child(widgets::caption(
                        format!("This PC: {} · port {}", peer::self_name(), hub.port),
                        cx,
                    ))
                    .child(div().flex_1())
                    .child(div().w_96().child(Input::new(&self.address).small()))
                    .child(
                        Button::new("peer-add")
                            .small()
                            .outline()
                            .label("Add")
                            .on_click(cx.listener(|this, _, _, cx| this.add_address(cx))),
                    )
                    .child(
                        Button::new("peers-off")
                            .small()
                            .ghost()
                            .label("Turn off")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.store.update(cx, |s, cx| s.set_peer_sync(false, cx))
                            })),
                    ),
            )
            .when_some(discovery, |c, e| c.child(widgets::caption(e, cx).text_xs()))
            .children(message)
            .child(peers)
            .children(panel)
            .into_any_element()
    }
}
