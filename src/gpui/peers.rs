//! An instance's Peers tab: other PCs running Hopper (LAN, Tailscale or by
//! address), pairing with a code, and pulling or pushing the instance's
//! files with a paired peer.

use crate::store::Store;
use crate::widgets;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::radio::Radio;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{Selectable as _, 
    ActiveTheme as _, Disableable as _, Icon, Sizable as _, StyledExt as _, WindowExt as _,
    h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use hopper::peer::{self, DiffEntry, Hub, SharedInst, Status};
use hopper::settings::FollowedRecipe;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// The invite code just made, and when it stops working.
#[derive(Clone)]
struct Invite {
    code: String,
    until: Instant,
}

/// "Works once. Expires in 14:32." from the mailbox's own lifetime.
fn invite_left(inv: &Invite) -> String {
    let secs = inv.until.saturating_duration_since(Instant::now()).as_secs();
    format!("Works once. Expires in {:02}:{:02}.", secs / 60, secs % 60)
}

pub struct PeersView {
    store: Entity<Store>,
    root: Option<PathBuf>,
    address: Entity<InputState>,
    /// The friend's one-time code, for the mailbox.
    join_code: Entity<InputState>,
    /// The invite code just made, shown with a Copy button and copied once.
    invite: Option<Invite>,
    /// Bumped for each invite so an older countdown stops.
    invite_gen: u64,
    /// Drawn as the docked panel instead of the full page.
    docked: bool,
    message: Option<Result<String, String>>,
    /// Mods from other sources the last pull left out, said after its result.
    skipped: Option<String>,
    /// Codes to compare after pairing, by peer.
    codes: HashMap<String, String>,
    selected: Option<String>,
    remote: Option<Result<Vec<SharedInst>, String>>,
    remote_inst: String,
    folder: &'static str,
    diff: Option<Result<Vec<DiffEntry>, String>>,
    picked: HashSet<String>,
    running: bool,
    /// Show the address, port and discovery details.
    show_advanced: bool,
    /// Show the manual file-by-file sync under the recipe action.
    show_files: bool,
    _subscriptions: Vec<Subscription>,
}

impl PeersView {
    pub fn new(store: Entity<Store>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let address = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Add by address: 100.x.y.z, host:port or https URL")
        });
        let join_code = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Paste their invite code")
        });
        let subs = vec![
            cx.observe(&store, |_, _, cx| cx.notify()),
            cx.subscribe(&address, |this, _, e: &InputEvent, cx| {
                if matches!(e, InputEvent::PressEnter { .. }) {
                    this.add_address(cx);
                }
            }),
            cx.subscribe(&join_code, |this, _, e: &InputEvent, cx| {
                if matches!(e, InputEvent::PressEnter { .. }) {
                    this.mailbox_join(cx);
                }
            }),
        ];
        Self {
            store,
            root: None,
            address,
            join_code,
            invite: None,
            invite_gen: 0,
            docked: false,
            message: None,
            skipped: None,
            codes: HashMap::new(),
            selected: None,
            remote: None,
            remote_inst: String::new(),
            folder: "mods",
            diff: None,
            picked: HashSet::new(),
            running: false,
            show_advanced: false,
            show_files: false,
            _subscriptions: subs,
        }
    }

    pub fn show_for(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        if self.root.as_ref() == Some(&root) {
            return;
        }
        self.diff = None;
        self.picked.clear();
        self.remote_inst.clear();
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
                if this.show_files {
                    this.compare(cx);
                } else {
                    this.diff = None;
                }
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

    /// Pulls bringing mods from GitHub and other links ask first (Mod
    /// sources off, or a source not trusted yet); skipped ones stay there.
    fn transfer(&mut self, pull: bool, files: Vec<String>, cx: &mut Context<Self>) {
        let pins: Vec<hopper::sources::carry::Pin> = match (&self.diff, pull) {
            (Some(Ok(d)), true) => d
                .iter()
                .filter(|e| files.contains(&e.rel))
                .filter_map(|e| {
                    let source = e.remote.as_ref()?.source.clone()?;
                    // Already following that source here: nothing new.
                    (e.local.as_ref().and_then(|l| l.source.as_ref()).map(|s| s.key()) != Some(source.key()))
                        .then(|| hopper::sources::carry::Pin { file: e.rel.clone(), source })
                })
                .collect(),
            _ => Vec::new(),
        };
        let on = self.store.read(cx).settings().allow_sources;
        if pins.is_empty() || (on && hopper::sources::carry::new_sources(&pins).is_empty()) {
            self.transfer_now(pull, files, cx);
            return;
        }
        let (store, view) = (self.store.clone(), cx.entity());
        let Some(handle) = cx.active_window() else { return };
        cx.defer(move |cx| {
            handle
                .update(cx, |_, window, cx| {
                    crate::incoming_sources::gate(store, pins, window, cx, move |skip, _, cx| {
                        let files: Vec<String> = files.iter().filter(|f| !skip.iter().any(|p| &p.file == *f)).cloned().collect();
                        if let Some(n) = hopper::sources::carry::skipped_note(&skip) {
                            view.update(cx, |this, _| this.skipped = Some(n));
                        }
                        view.update(cx, |this, cx| this.transfer_now(pull, files, cx));
                    });
                })
                .ok();
        });
    }

    fn transfer_now(&mut self, pull: bool, files: Vec<String>, cx: &mut Context<Self>) {
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
                                "Pulled {} files. Undo it from History.",
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
                let skipped = this.skipped.take();
                this.message = Some(r.map(|m| match skipped {
                    Some(n) => format!("{m} {n}."),
                    None => m,
                }));
                this.compare(cx);
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn root_string(&self) -> String {
        self.root
            .as_ref()
            .map(|r| r.to_string_lossy().into_owned())
            .unwrap_or_default()
    }

    fn followed(&self, cx: &App) -> Option<FollowedRecipe> {
        let root = self.root.as_ref()?;
        self.store
            .read(cx)
            .settings()
            .followed_recipes
            .iter()
            .find(|f| std::path::Path::new(&f.root) == root)
            .cloned()
    }

    fn save_follow(&self, f: FollowedRecipe, cx: &mut Context<Self>) {
        let root = f.root.clone();
        self.store.update(cx, |s, cx| {
            s.update_settings(
                |st| {
                    st.followed_recipes.retain(|x| x.root != root);
                    st.followed_recipes.push(f.clone());
                },
                cx,
            );
        });
    }

    /// Fetches FOX's instance as a recipe and applies it to this instance.
    fn follow(&mut self, cx: &mut Context<Self>) {
        let (Some(hub), Some(id), Some(root)) =
            (self.hub(cx), self.selected.clone(), self.root.clone())
        else {
            return;
        };
        if self.remote_inst.is_empty() {
            return;
        }
        let rinst = self.remote_inst.clone();
        let peer_name = hub
            .peers()
            .into_iter()
            .find(|p| p.peer_id == id)
            .map(|p| p.name)
            .unwrap_or_default();
        self.running = true;
        self.message = None;
        cx.spawn(async move |this, cx| {
            let (id2, rinst2, root2) = (id.clone(), rinst.clone(), root.clone());
            let r = cx
                .background_spawn(async move {
                    let recipe = hub.recipe(&id2, &rinst2)?;
                    let share = hopper::sharecode::decode(&recipe.code)?;
                    let inst = hopper::instances::load(&root2).ok_or_else(|| {
                        anyhow::anyhow!("{} isn't an instance any more", root2.display())
                    })?;
                    Ok::<_, anyhow::Error>((recipe, hopper::recipe::fit(&inst, &share)))
                })
                .await;
            this.update(cx, |this, cx| {
                this.running = false;
                match r {
                    Err(e) => this.message = Some(Err(format!("{e:#}"))),
                    Ok((_, hopper::recipe::Fit::Different(why))) => {
                        this.message = Some(Err(why))
                    }
                    Ok((recipe, _)) => {
                        this.save_follow(
                            FollowedRecipe {
                                peer_id: id.clone(),
                                peer_name: peer_name.clone(),
                                inst: rinst.clone(),
                                name: recipe.name.clone(),
                                root: root.to_string_lossy().into_owned(),
                                code: recipe.code.clone(),
                                seen: recipe.code.clone(),
                                left_out: recipe.left_out.clone(),
                            },
                            cx,
                        );
                        let (code, root_j) = (recipe.code.clone(), root.clone());
                        let root_arg = root_j.clone();
                        let title = format!("Following {peer_name}'s {}", recipe.name);
                        this.store.read(cx).spawn_instance_job(
                            &root_arg,
                            title,
                            move |job| {
                                hopper::recipe::apply_to(&root_j, &code, job)?;
                                Ok(crate::store::Msg::InstanceChanged(root_j.clone()))
                            },
                        );
                        this.message = Some(Ok(format!(
                            "Following {peer_name}'s {}. The mods are downloading.",
                            recipe.name
                        )));
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// Asks the followed PC whether its instance changed.
    fn check_follow(&mut self, cx: &mut Context<Self>) {
        let (Some(hub), Some(f)) = (self.hub(cx), self.followed(cx)) else {
            return;
        };
        self.running = true;
        self.message = None;
        cx.spawn(async move |this, cx| {
            let (peer, inst) = (f.peer_id.clone(), f.inst.clone());
            let r = cx
                .background_spawn(async move { hub.recipe(&peer, &inst) })
                .await;
            this.update(cx, |this, cx| {
                this.running = false;
                match r {
                    Err(e) => this.message = Some(Err(format!("{e:#}"))),
                    Ok(recipe) => {
                        let Some(mut f) = this.followed(cx) else { return };
                        let changed = recipe.code != f.code;
                        f.seen = recipe.code;
                        f.left_out = recipe.left_out;
                        let name = f.peer_name.clone();
                        this.save_follow(f, cx);
                        this.message = Some(Ok(if changed {
                            format!("{name} changed their instance. Update to apply it.")
                        } else {
                            "You're up to date.".to_string()
                        }));
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// Fetches the newest recipe and applies it to this instance.
    fn update_follow(&mut self, cx: &mut Context<Self>) {
        let (Some(hub), Some(f), Some(root)) =
            (self.hub(cx), self.followed(cx), self.root.clone())
        else {
            return;
        };
        self.running = true;
        self.message = None;
        cx.spawn(async move |this, cx| {
            let (peer, inst) = (f.peer_id.clone(), f.inst.clone());
            let r = cx
                .background_spawn(async move { hub.recipe(&peer, &inst) })
                .await;
            this.update(cx, |this, cx| {
                this.running = false;
                match r {
                    Err(e) => this.message = Some(Err(format!("{e:#}"))),
                    Ok(recipe) => {
                        let Some(mut f) = this.followed(cx) else { return };
                        f.code = recipe.code.clone();
                        f.seen = recipe.code.clone();
                        f.left_out = recipe.left_out.clone();
                        let (name, inst_name) = (f.peer_name.clone(), recipe.name.clone());
                        this.save_follow(f, cx);
                        let (code, root_j) = (recipe.code.clone(), root.clone());
                        let root_arg = root_j.clone();
                        this.store.read(cx).spawn_instance_job(
                            &root_arg,
                            format!("Updating from {name}'s {inst_name}"),
                            move |job| {
                                hopper::recipe::apply_to(&root_j, &code, job)?;
                                Ok(crate::store::Msg::InstanceChanged(root_j.clone()))
                            },
                        );
                        this.message =
                            Some(Ok(format!("Updating from {name}'s {inst_name}.")));
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn stop_follow(&mut self, cx: &mut Context<Self>) {
        let root = self.root_string();
        self.store.update(cx, |s, cx| {
            s.update_settings(
                |st| st.followed_recipes.retain(|f| f.root != root),
                cx,
            );
        });
        self.message = Some(Ok("Stopped following this instance.".into()));
        cx.notify();
    }

    fn follow_line(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let f = self.followed(cx)?;
        let theme = cx.theme();
        let changed = f.seen != f.code;
        Some(
            v_flex()
                .gap_1()
                .py_2()
                .border_b_1()
                .border_color(theme.border)
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            div()
                                .text_sm()
                                .font_semibold()
                                .child(format!("Following {}'s {}", f.peer_name, f.name)),
                        )
                        .child(if changed {
                            widgets::chip(theme.warning)
                                .xsmall()
                                .child("Update available")
                        } else {
                            widgets::chip(theme.success)
                                .xsmall()
                                .child("Up to date")
                        })
                        .child(div().flex_1())
                        .child(
                            Button::new("follow-check")
                                .xsmall()
                                .ghost()
                                .label("Check")
                                .disabled(self.running)
                                .on_click(cx.listener(|this, _, _, cx| this.check_follow(cx))),
                        )
                        .when(changed, |r| {
                            r.child(
                                Button::new("follow-update")
                                    .xsmall()
                                    .primary()
                                    .label("Update")
                                    .disabled(self.running)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.update_follow(cx)
                                    })),
                            )
                        })
                        .child(
                            Button::new("follow-stop")
                                .xsmall()
                                .ghost()
                                .label("Stop")
                                .on_click(cx.listener(|this, _, _, cx| this.stop_follow(cx))),
                        ),
                )
                .when(!f.left_out.is_empty(), |c| {
                    c.child(
                        widgets::caption(
                            format!(
                                "{} file{} can't be downloaded from any site. Ask them to send those.",
                                f.left_out.len(),
                                if f.left_out.len() == 1 { "" } else { "s" }
                            ),
                            cx,
                        )
                        .text_xs(),
                    )
                })
                .into_any_element(),
        )
    }

    /// Drawn as the docked panel (full height, beside the page).
    pub fn set_docked(&mut self, docked: bool) {
        self.docked = docked;
    }

    /// The Friends panel docked beside an instance page: the same card as
    /// the Friends tab, without the page's title and network section.
    pub fn panel_body(&mut self, cx: &mut Context<Self>) -> AnyElement {
        div()
            .id("friends-panel")
            .size_full()
            .overflow_y_scrollbar()
            .child(div().p_3().child(self.mailbox_card(cx)))
            .into_any_element()
    }

    fn set_shared(&mut self, key: String, friend: String, on: bool, cx: &mut Context<Self>) {
        self.store.update(cx, |s, cx| {
            s.update_settings(
                |st| {
                    st.set_shared(&key, &friend, on);
                    st.share_prompt_done = true;
                },
                cx,
            )
        });
        cx.notify();
    }

    fn share_all(&mut self, cx: &mut Context<Self>) {
        let keys: Vec<String> = self
            .store
            .read(cx)
            .instances()
            .iter()
            .filter_map(|i| i.root.file_name().map(|n| n.to_string_lossy().into_owned()))
            .collect();
        let friends: Vec<String> = self
            .store
            .read(cx)
            .mailbox_friends()
            .iter()
            .map(|f| f.id.clone())
            .collect();
        self.store.update(cx, |s, cx| {
            s.update_settings(
                |st| {
                    st.share_with_all(&keys, &friends);
                    st.share_prompt_done = true;
                },
                cx,
            )
        });
        cx.notify();
    }

    fn dismiss_share_prompt(&mut self, cx: &mut Context<Self>) {
        self.store.update(cx, |s, cx| {
            s.update_settings(|st| st.share_prompt_done = true, cx)
        });
        cx.notify();
    }

    fn mailbox_card(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let mb = self.store.read(cx).mailbox();
        let friends = self.store.read(cx).mailbox_friends().to_vec();
        let recipes = self.store.read(cx).mailbox_recipes().to_vec();
        let err = self.store.read(cx).mailbox_error();
        let has_friends = !friends.is_empty();
        let instances = self.store.read(cx).instances().to_vec();
        let shared = self.store.read(cx).settings().shared_with.clone();
        let prompt = has_friends && shared.is_empty() && !self.store.read(cx).settings().share_prompt_done;
        // One section per friend. A friend who sent a pack but isn't paired
        // any more still gets one, so their pack doesn't vanish.
        let mut groups: Vec<(String, String, String, bool)> = friends
            .iter()
            .map(|f| (f.id.clone(), f.name.clone(), f.mc_id.clone(), f.online))
            .collect();
        for e in &recipes {
            if !groups.iter().any(|g| g.0 == e.from) {
                groups.push((e.from.clone(), e.from_name.clone(), String::new(), false));
            }
        }
        let groups_len = groups.len();
        v_flex()
            .gap_2()
            .p_3()
            .rounded(theme.radius)
            .border_1()
            .border_color(theme.border)
            .child(
                h_flex()
                    .gap_2()
                    .child(div().text_sm().font_semibold().child("From anywhere"))
                    .when(mb.is_none() && err.is_none(), |r| {
                        r.child(Spinner::new().small())
                    })
                    .child(div().flex_1())
                    .when(mb.is_some(), |r| {
                        r.child(
                            Button::new("mb-invite")
                                .small()
                                .outline()
                                .label("Invite a friend")
                                .on_click(cx.listener(|this, _, _, cx| this.mailbox_invite(cx))),
                        )
                    }),
            )
            .when_some(err, |c, e| {
                c.child(div().text_sm().text_color(theme.danger).child(e))
            })
            .when(mb.is_some() && friends.is_empty() && recipes.is_empty(), |c| {
                c.child(widgets::caption(
                    "Invite a friend to get a code, or paste the code they sent you. Codes work once and run out after 15 minutes.",
                    cx,
                ))
            })
            .when(prompt, |c| {
                let theme = cx.theme().clone();
                c.child(
                    v_flex()
                        .gap_1()
                        .p_2()
                        .rounded(theme.radius)
                        .bg(theme.muted)
                        .child(div().text_sm().font_medium().child("Your friends can't see your packs yet."))
                        .child(widgets::caption(
                            "Turn on the packs you want to share. Only the mod list goes to them; files download from their sites.",
                            cx,
                        ))
                        .child(
                            h_flex()
                                .gap_2()
                                .pt_1()
                                .child(
                                    Button::new("mb-share-all")
                                        .xsmall()
                                        .primary()
                                        .label("Share all")
                                        .on_click(cx.listener(|this, _, _, cx| this.share_all(cx))),
                                )
                                .child(
                                    Button::new("mb-share-not")
                                        .xsmall()
                                        .ghost()
                                        .label("Not now")
                                        .on_click(cx.listener(|this, _, _, cx| this.dismiss_share_prompt(cx))),
                                ),
                        ),
                )
            })
            .children(groups.into_iter().enumerate().map(|(gi, (fid, fname, mc_id, online))| {
                let fname = if fname.is_empty() { "A friend".to_string() } else { fname };
                let theirs: Vec<hopper::mailbox::Event> =
                    recipes.iter().filter(|e| e.from == fid).cloned().collect();
                let head = if mc_id.is_empty() {
                    div()
                        .size_2()
                        .rounded_full()
                        .bg(if online { theme.success } else { theme.muted_foreground })
                        .into_any_element()
                } else {
                    let saved = hopper::accounts::Saved {
                        id: mc_id.clone(),
                        name: fname.clone(),
                        ..Default::default()
                    };
                    let url = saved.fallback_avatar();
                    self.store.update(cx, |s, _| s.want_image_now(&url));
                    crate::accounts::head(&self.store.read(cx), &saved, rems(1.5), cx)
                };
                v_flex()
                    .gap_1()
                    .py_2()
                    .when(gi > 0, |r| {
                        r.pt_3().border_t_1().border_color(theme.border)
                    })
                    .when(gi == 0 && groups_len > 0, |r| r.pt_1())
                    .child(
                        h_flex()
                            .gap_2()
                            .px_1()
                            .items_center()
                            .child(head)
                            .child(div().text_sm().font_medium().child(fname.clone()))
                            .when(online && !mc_id.is_empty(), |r| {
                                r.child(div().size_2().rounded_full().bg(theme.success))
                            })
                            .child(widgets::chip(theme.muted_foreground).xsmall().child("Paired")),
                    )
                    .when(!theirs.is_empty(), |c| {
                        c.child(div().pt_1().px_1().child(widgets::caption("Their packs", cx)))
                            .children(theirs.into_iter().map(|e| {
                                let already = self
                                    .store
                                    .read(cx)
                                    .settings()
                                    .followed_recipes
                                    .iter()
                                    .any(|f| f.peer_id == e.from && f.inst == e.inst);
                                let pack = if e.name.is_empty() { e.inst.clone() } else { e.name.clone() };
                                h_flex()
                                    .gap_2()
                                    .px_1()
                                    .child(div().text_sm().flex_1().min_w_0().truncate().child(pack))
                                    .when(!e.left_out.is_empty(), |r| {
                                        r.child(
                                            widgets::caption(
                                                format!("{} file(s) aren't downloadable", e.left_out.len()),
                                                cx,
                                            )
                                            .text_xs(),
                                        )
                                    })
                                    .when(already, |r| {
                                        r.child(widgets::chip(theme.success).xsmall().child("Following"))
                                    })
                                    .when(!already, |r| {
                                        r.child(
                                            Button::new(SharedString::from(format!(
                                                "mb-get:{}:{}",
                                                e.from, e.inst
                                            )))
                                            .xsmall()
                                            .outline()
                                            .label("Follow")
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.mailbox_follow(&e, cx)
                                            })),
                                        )
                                    })
                            }))
                    })
                    .when(!instances.is_empty(), |c| {
                        c.child(
                            div().pt_1().px_1().child(widgets::caption("Your packs", cx)),
                        )
                        .children(instances.iter().map(|inst| {
                            let key = inst
                                .root
                                .file_name()
                                .unwrap_or_default()
                                .to_string_lossy()
                                .into_owned();
                            let on = shared
                                .iter()
                                .any(|p| p.friend == fid && p.key.eq_ignore_ascii_case(&key));
                            let name = inst.name.clone();
                            let (k2, f2) = (key.clone(), fid.clone());
                            h_flex()
                                .gap_2()
                                .px_1()
                                .items_center()
                                .child(div().text_sm().flex_1().min_w_0().truncate().child(name))
                                .child(
                                    widgets::caption(if on { "Shared" } else { "Private" }, cx)
                                        .text_xs()
                                        .text_color(if on {
                                            theme.success
                                        } else {
                                            theme.muted_foreground
                                        }),
                                )
                                .child(
                                    Switch::new(SharedString::from(format!(
                                        "mb-share:{key}:{fid}"
                                    )))
                                    .small()
                                    .checked(on)
                                    .tooltip(format!("Share this pack with {fname}. Only the mod list goes to them."))
                                    .on_change(cx.listener(move |this, v: &bool, _, cx| {
                                        this.set_shared(k2.clone(), f2.clone(), *v, cx)
                                    })),
                                )
                        }))
                    })
            }))
            .when_some(self.invite.clone(), |c, inv| {
                let copy = inv.code.clone();
                c.child(
                    v_flex()
                        .gap_1()
                        .px_1()
                        .pt_2()
                        .border_t_1()
                        .border_color(theme.border)
                        .child(
                            h_flex()
                                .gap_2()
                                .items_center()
                                .child(
                                    widgets::chip(theme.primary)
                                        .text_sm()
                                        .font_semibold()
                                        .child(inv.code.clone()),
                                )
                                .child(
                                    Button::new("mb-copy")
                                        .xsmall()
                                        .outline()
                                        .label("Copy")
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()));
                                            this.message = Some(Ok("Code copied.".into()));
                                            cx.notify();
                                        })),
                                ),
                        )
                        .child(widgets::caption(invite_left(&inv), cx).text_xs()),
                )
            })
            .when(mb.is_some(), |c| {
                c.child(
                    v_flex()
                        .gap_1()
                        .px_1()
                        .pt_2()
                        .when(self.invite.is_none(), |r| {
                            r.border_t_1().border_color(theme.border)
                        })
                        .child(widgets::caption("Have a code from them?", cx))
                        .child(
                            h_flex()
                                .gap_2()
                                .items_center()
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .child(Input::new(&self.join_code).small()),
                                )
                                .child(
                                    Button::new("mb-join")
                                        .small()
                                        .outline()
                                        .label("Join")
                                        .disabled(self.running)
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.mailbox_join(cx)
                                        })),
                                ),
                        ),
                )
            })
            .into_any_element()
    }

    fn mailbox_invite(&mut self, cx: &mut Context<Self>) {
        let Some(mb) = self.store.read(cx).mailbox() else {
            return;
        };
        self.running = true;
        self.message = None;
        cx.spawn(async move |this, cx| {
            let r = cx.background_spawn(async move { mb.invite() }).await;
            this.update(cx, |this, cx| {
                this.running = false;
                this.message = Some(match r {
                    Ok((code, secs)) => {
                        // Put it on the clipboard straight away; the Copy
                        // button beside it is for later.
                        cx.write_to_clipboard(ClipboardItem::new_string(code.clone()));
                        this.invite = Some(Invite {
                            code,
                            until: Instant::now() + Duration::from_secs(secs),
                        });
                        this.start_invite_ticker(cx);
                        Ok("Code copied. Send it to your friend; it works once.".to_string())
                    }
                    Err(e) => Err(format!("{e:#}")),
                });
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// Keeps the invite's "Expires in" line moving, then clears it. It runs
    /// while the view is open, so changing tabs or instance doesn't lose it.
    fn start_invite_ticker(&mut self, cx: &mut Context<Self>) {
        self.invite_gen = self.invite_gen.wrapping_add(1);
        let id = self.invite_gen;
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let more = this
                    .update(cx, |this, cx| {
                        if this.invite_gen != id {
                            return false;
                        }
                        let Some(inv) = &this.invite else {
                            return false;
                        };
                        if Instant::now() >= inv.until {
                            this.invite = None;
                            this.message =
                                Some(Ok("That code expired. Invite a friend again for a new one.".into()));
                        }
                        cx.notify();
                        this.invite.is_some()
                    })
                    .unwrap_or(false);
                if !more {
                    break;
                }
            }
        })
        .detach();
    }

    fn mailbox_join(&mut self, cx: &mut Context<Self>) {
        let code = self.join_code.read(cx).value().trim().to_string();
        if code.is_empty() {
            return;
        }
        let Some(mb) = self.store.read(cx).mailbox() else {
            return;
        };
        self.running = true;
        self.message = None;
        cx.spawn(async move |this, cx| {
            let r = cx.background_spawn(async move { mb.join(&code) }).await;
            this.update(cx, |this, cx| {
                this.running = false;
                this.message = Some(match r {
                    Ok(f) => Ok(format!(
                        "Paired with {}. Their instances appear here when they share one.",
                        if f.name.is_empty() { "them" } else { &f.name }
                    )),
                    Err(e) => Err(format!("{e:#}")),
                });
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn mailbox_follow(&mut self, e: &hopper::mailbox::Event, cx: &mut Context<Self>) {
        let share = match hopper::sharecode::decode(&e.code) {
            Ok(s) => s,
            Err(err) => {
                self.message = Some(Err(format!("{err:#}")));
                cx.notify();
                return;
            }
        };
        // Only instances this code fits: the same pack, or a newer version
        // of it. A different pack has to be a new instance.
        let mut fitting = Vec::new();
        for inst in self.store.read(cx).instances() {
            let update = match hopper::recipe::fit(inst, &share) {
                hopper::recipe::Fit::Different(_) => continue,
                hopper::recipe::Fit::PackUpdate(_) => true,
                hopper::recipe::Fit::Same => false,
            };
            fitting.push((inst.root.clone(), inst.name.clone(), update));
        }
        let preferred = self
            .root
            .as_ref()
            .and_then(|r| fitting.iter().position(|(root, _, _)| root == r))
            .unwrap_or(0);
        let Some(handle) = cx.active_window() else {
            return;
        };
        let e = e.clone();
        let store = self.store.clone();
        let title = format!(
            "Follow {}'s {}",
            if e.from_name.is_empty() { "a friend" } else { &e.from_name },
            if e.name.is_empty() { &e.inst } else { &e.name }
        );
        // Opening a dialog from inside this view's click handler is too
        // early; wait for the window's next update like the other dialogs.
        cx.defer(move |cx| {
            handle
                .update(cx, |_, window, cx| {
                    let view = cx.new(|cx| FollowChooser::new(&e, share, fitting, preferred, cx));
                    window.open_dialog(cx, move |dialog, window, _| {
                        let (f2, s2) = (view.clone(), store.clone());
                        dialog
                            .title(title.clone())
                            .w(rems(40.).to_pixels(window.rem_size()))
                            .margin_top(rems(widgets::DIALOG_TOP).to_pixels(window.rem_size()))
                            .child(view.clone())
                            .footer(crate::dialogs::footer("Follow", true))
                            .on_ok(move |_, window, cx| {
                                FollowChooser::go(&f2, &s2, window, cx)
                            })
                    });
                })
                .ok();
        });
    }

    fn intro(&self, cx: &mut Context<Self>) -> AnyElement {
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap_3()
            .px_6()
            .child(Icon::new(IconName::Users).large().text_color(cx.theme().muted_foreground))
            .child(div().text_lg().font_semibold().child("Sync with a friend"))
            .child(
                div().max_w(rems(38.)).text_sm().text_center().child(
                    "Get their exact instance here, or share yours. Only the list of mods is sent — the files download from CurseForge, Modrinth and GitHub, so nothing large goes between you.",
                ),
            )
            .child(
                widgets::caption(
                    "No account. Nothing is uploaded to Hopper. Windows may ask to allow Hopper on private networks; allow it so the other PC can reach this one.",
                    cx,
                )
                .text_xs(),
            )
            .child(
                Button::new("peers-on")
                    .primary()
                    .icon(IconName::Users)
                    .label("Turn on sync")
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
        let following = self.followed(cx);
        let (empty_title, empty_body) = match &following {
            Some(f) => (
                format!("{}'s PC isn't here right now", f.peer_name),
                "You can still check for updates when their PC is online.".to_string(),
            ),
            None => (
                "No friend's PC here yet".to_string(),
                "Turn on sync on their PC too. Both PCs need to be on the same network or Tailscale."
                    .to_string(),
            ),
        };
        v_flex()
            .gap_1()
            .children(peers.into_iter().map(|p| {
                let online = p.last_seen > 0;
                let active = self.selected.as_deref() == Some(p.peer_id.as_str());
                let (id1, id2, id3) = (p.peer_id.clone(), p.peer_id.clone(), p.peer_id.clone());
                let source = match p.source.as_str() {
                    "lan" => "Nearby",
                    "tailscale" => "Tailscale",
                    "address" => "Added by address",
                    other => other,
                };
                h_flex()
                    .gap_3()
                    .px_3()
                    .py_2()
                    .rounded(theme.radius)
                    .when(active, |r| widgets::row_selected(r, cx))
                    .child(
                        div()
                            .size_2()
                            .rounded_full()
                            .bg(if online { theme.success } else { theme.muted_foreground }),
                    )
                    .child(
                        v_flex()
                            .gap_0p5()
                            .child(
                                h_flex()
                                    .gap_2()
                                    .child(div().text_sm().font_semibold().child(p.name.clone()))
                                    .when(!source.is_empty(), |r| {
                                        r.child(
                                            widgets::chip(theme.muted_foreground)
                                                .xsmall()
                                                .child(source.to_string()),
                                        )
                                    }),
                            )
                            .when(self.show_advanced, |r| {
                                r.child(widgets::caption(p.addr.clone(), cx).text_xs())
                            }),
                    )
                    .child(div().flex_1())
                    .when_some(self.codes.get(&p.peer_id).cloned(), |r, c| {
                        r.child(
                            div()
                                .text_sm()
                                .child(format!("Their screen should show {c}")),
                        )
                    })
                    .map(|r| {
                        if p.trusted {
                            r.child(widgets::chip(theme.success).child("Paired")).child(
                                Button::new(SharedString::from(format!("sync:{id1}")))
                                    .small()
                                    .outline()
                                    .selected(active)
                                    .label("See their instances")
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
                                    .outline()
                                    .label("Pair")
                                    .tooltip("Pair once, then check the 6-digit code matches on their screen")
                                    .disabled(!online)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.pair(id3.clone(), cx)
                                    })),
                            )
                        }
                    })
            }))
            .when(empty, |c| {
                c.child(
                    v_flex()
                        .items_start()
                        .gap_3()
                        .p_4()
                        .rounded(theme.radius)
                        .border_1()
                        .border_color(theme.border)
                        .child(
                            h_flex()
                                .gap_3()
                                .items_start()
                                .child(
                                    Icon::new(IconName::Users)
                                        .small()
                                        .text_color(theme.muted_foreground),
                                )
                                .child(
                                    v_flex()
                                        .gap_1()
                                        .child(
                                            div()
                                                .text_sm()
                                                .font_semibold()
                                                .child(empty_title),
                                        )
                                        .child(widgets::caption(empty_body, cx)),
                                ),
                        )
                        .child(
                            Button::new("peer-address")
                                .small()
                                .outline()
                                .label("Pair by address")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.show_advanced = true;
                                    cx.notify();
                                })),
                        )
                        .child(widgets::caption(
                            "No account. Nothing is uploaded to Hopper.",
                            cx,
                        ).text_xs()),
                )
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
                                    .tooltip("Send these files to their PC; they can undo it from History")
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
                                    Status::Differ => widgets::chip(cx.theme().info).xsmall().child("Different"),
                                    Status::OnlyLocal => widgets::chip(cx.theme().warning).xsmall().child("Only here"),
                                    Status::OnlyRemote => widgets::chip(cx.theme().primary).xsmall().child("Only theirs"),
                                    Status::Same => widgets::chip(cx.theme().muted_foreground).xsmall().child("Same"),
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
                        .when(self.show_files, |r| {
                            r.child(
                                widgets::Segmented::new("peer-folder")
                                    .selected_index(folder_ix)
                                    .children(peer::FOLDERS.iter().map(|f| widgets::SegItem::new().label(format!("{f}/"))))
                                    .on_click(cx.listener(|this, ix: &usize, _, cx| {
                                        this.folder = peer::FOLDERS[*ix];
                                        this.compare(cx);
                                    })),
                            )
                        })
                        .when(self.show_files, |r| {
                            r.child(
                                Button::new("peer-compare")
                                    .small()
                                    .ghost()
                                    .icon(IconName::RefreshCw)
                                    .tooltip("Compare again")
                                    .disabled(self.running)
                                    .on_click(cx.listener(|this, _, _, cx| this.compare(cx))),
                            )
                        })
                        .child(div().flex_1())
                        .when(self.followed(cx).is_none(), |r| {
                            r.child(
                                Button::new("peer-follow")
                                    .small()
                                    .primary()
                                    .label("Follow this instance")
                                    .tooltip("Match this instance to theirs. Mods download from their original sites.")
                                    .disabled(self.running)
                                    .on_click(cx.listener(|this, _, _, cx| this.follow(cx))),
                            )
                        })
                        .child(
                            Button::new("peer-files")
                                .small()
                                .ghost()
                                .label(if self.show_files { "Hide files" } else { "Files…" })
                                .tooltip("Copy individual files instead of following the whole instance")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.show_files = !this.show_files;
                                    if this.show_files {
                                        this.compare(cx);
                                    } else {
                                        this.diff = None;
                                    }
                                    cx.notify();
                                })),
                        ),
                )
                .when(self.show_files, |c| c.child(diff))
                .when(!self.show_files, |c| {
                    c.child(widgets::caption(
                        "Only the list of mods is sent. The files download from CurseForge, Modrinth and GitHub.",
                        cx,
                    ))
                })
                .into_any_element(),
        )
    }
}

/// What the Follow dialog picked.
#[derive(Clone, Debug, PartialEq)]
enum FollowPick {
    Existing(usize),
    New,
}

/// Asks where a friend's pack should go: over an instance that already
/// fits it, or as a new one. Following replaces the mods, so this is never
/// done without asking.
#[derive(Clone)]
struct FollowChooser {
    peer_id: String,
    peer_name: String,
    /// The sender's instance key.
    inst: String,
    name: String,
    code: String,
    share: hopper::sharecode::Share,
    left_out: Vec<String>,
    /// Local instances the code fits, and whether their pack updates first.
    fitting: Vec<(PathBuf, String, bool)>,
    pick: FollowPick,
}

impl FollowChooser {
    fn new(
        e: &hopper::mailbox::Event,
        share: hopper::sharecode::Share,
        fitting: Vec<(PathBuf, String, bool)>,
        preferred: usize,
        _cx: &mut Context<Self>,
    ) -> Self {
        let pick = if fitting.is_empty() {
            FollowPick::New
        } else {
            FollowPick::Existing(preferred.min(fitting.len() - 1))
        };
        Self {
            peer_id: e.from.clone(),
            peer_name: e.from_name.clone(),
            inst: e.inst.clone(),
            name: e.name.clone(),
            code: e.code.clone(),
            share,
            left_out: e.left_out.clone(),
            fitting,
            pick,
        }
    }

    fn recipe(&self) -> FollowedRecipe {
        FollowedRecipe {
            peer_id: self.peer_id.clone(),
            peer_name: self.peer_name.clone(),
            inst: self.inst.clone(),
            name: self.name.clone(),
            root: String::new(),
            code: self.code.clone(),
            seen: self.code.clone(),
            left_out: self.left_out.clone(),
        }
    }

    fn go(view: &Entity<Self>, store: &Entity<Store>, _: &mut Window, cx: &mut App) -> bool {
        let this = view.read(cx).clone();
        let title = format!("Following {}'s {}", this.peer_name, this.name);
        match this.pick {
            FollowPick::Existing(ix) => {
                let Some((root, _, _)) = this.fitting.get(ix) else {
                    return true;
                };
                let root = root.clone();
                let code = this.code.clone();
                let mut recipe = this.recipe();
                recipe.root = root.to_string_lossy().into_owned();
                store.update(cx, |s, cx| {
                    s.update_settings(
                        |st| {
                            // What was applied last stays, so the instance
                            // can still say an update is waiting.
                            if let Some(old) =
                                st.followed_recipes.iter().find(|x| x.root == recipe.root)
                            {
                                recipe.code = old.code.clone();
                            }
                            st.followed_recipes.retain(|x| x.root != recipe.root);
                            st.followed_recipes.push(recipe.clone());
                        },
                        cx,
                    );
                    s.apply_follow_now(root, code, title, cx);
                });
            }
            FollowPick::New => {
                let root = PathBuf::from(store.read(cx).settings().instances_root.clone());
                let share = this.share.clone();
                let recipe = this.recipe();
                store.read(cx).spawn_job(title, move |job| {
                    job.announce();
                    let inst = hopper::sharecode::install_share(&root, &share, job)?;
                    job.detail(format!("Installed {}", inst.name));
                    Ok(crate::store::Msg::Followed(Box::new(FollowedRecipe {
                        root: inst.root.to_string_lossy().into_owned(),
                        ..recipe
                    })))
                });
            }
        }
        true
    }
}

impl Render for FollowChooser {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let who = if self.peer_name.is_empty() {
            "A friend".to_string()
        } else {
            self.peer_name.clone()
        };
        let pack = if self.name.is_empty() {
            self.inst.clone()
        } else {
            self.name.clone()
        };
        let mut body = v_flex()
            .gap_3()
            .child(div().text_sm().child(format!(
                "{who} shared {pack} with you. Following it keeps this PC in step with theirs."
            )))
            .when(!self.left_out.is_empty(), |c| {
                c.child(
                    widgets::caption(
                        format!(
                            "{} file(s) can't be downloaded from any site. Ask them to send those.",
                            self.left_out.len()
                        ),
                        cx,
                    )
                    .text_xs(),
                )
            })
            .child(widgets::caption(
                if self.fitting.is_empty() {
                    "None of your packs match theirs, so this starts a new one."
                } else {
                    "Where should it go?"
                },
                cx,
            ));
        let fitting = self.fitting.clone();
        body = body.children(fitting.iter().enumerate().map(|(ix, (_, name, updates))| {
            let picked = self.pick == FollowPick::Existing(ix);
            v_flex()
                .gap_1()
                .child(
                    Radio::new(SharedString::from(format!("follow-here:{ix}")))
                        .label(name.clone())
                        .checked(picked)
                        .on_click(cx.listener(move |this, _: &bool, _, cx| {
                            this.pick = FollowPick::Existing(ix);
                            cx.notify();
                        })),
                )
                .when(*updates, |c| {
                    c.child(
                        widgets::caption(
                            "Their pack is a newer version. Your pack updates first, then the mods.",
                            cx,
                        )
                        .text_xs()
                        .pl_6(),
                    )
                })
        }));
        body
            .child(
                v_flex()
                    .gap_1()
                    .child(
                        Radio::new("follow-new")
                            .label(format!("Start a new instance: {pack}"))
                            .checked(self.pick == FollowPick::New)
                            .on_click(cx.listener(|this, _: &bool, _, cx| {
                                this.pick = FollowPick::New;
                                cx.notify();
                            })),
                    )
                    .child(
                        widgets::caption(
                            "Their pack and mods install as a new instance. Nothing here changes.",
                            cx,
                        )
                        .text_xs()
                        .pl_6(),
                    ),
            )
            .when(matches!(self.pick, FollowPick::Existing(_)), |c| {
                c.child(
                    div()
                        .text_xs()
                        .text_color(theme.warning)
                        .child("Your mods and settings there are replaced with theirs. History can undo it."),
                )
            })
    }
}

impl Render for PeersView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.docked {
            return div().size_full().child(self.panel_body(cx)).into_any_element();
        }
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
        let follow = self.follow_line(cx);
        let theme = cx.theme();
        let advanced: AnyElement = if self.show_advanced {
            v_flex()
                .gap_2()
                .pt_2()
                .border_t_1()
                .border_color(theme.border)
                .child(widgets::caption(
                    format!("This PC: {} · port {}", peer::self_name(), hub.port),
                    cx,
                ))
                .child(
                    h_flex()
                        .gap_2()
                        .child(div().w_96().child(Input::new(&self.address).small()))
                        .child(
                            Button::new("peer-add")
                                .small()
                                .outline()
                                .label("Add")
                                .on_click(cx.listener(|this, _, _, cx| this.add_address(cx))),
                        ),
                )
                .when_some(discovery, |c, e| {
                    c.child(
                        widgets::caption(
                            "Hopper can't find PCs on this network. Pair by address instead.",
                            cx,
                        )
                        .text_xs(),
                    )
                    .child(widgets::caption(e, cx).text_xs())
                })
                .child(
                    widgets::caption(
                        "Windows may ask to allow Hopper on private networks; allow it so the other PC can reach this one.",
                        cx,
                    )
                    .text_xs(),
                )
                .child(
                    Button::new("peer-adv-off")
                        .small()
                        .ghost()
                        .label("Hide advanced")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.show_advanced = false;
                            cx.notify();
                        })),
                )
                .into_any_element()
        } else {
            h_flex().child(
                Button::new("peer-adv")
                    .small()
                    .ghost()
                    .label("Advanced: address and network")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.show_advanced = true;
                        cx.notify();
                    })),
            )
            .into_any_element()
        };
        // The page is taller than the window once there are friends and a
        // library, so it scrolls as a whole.
        div()
            .id("peers-scroll")
            .size_full()
            .overflow_y_scrollbar()
            .child(
        v_flex()
            .w_full()
            .px_6()
            .pb_6()
            .gap_4()
            .child(
                h_flex()
                    .gap_3()
                    .child(
                        v_flex()
                            .gap_0p5()
                            .child(div().text_lg().font_semibold().child("Sync with a friend"))
                            .child(widgets::caption(
                                "Share this instance with a friend's PC. Only the list of mods is sent; files download from their original sites.",
                                cx,
                            )),
                    )
                    .child(div().flex_1())
                    .child(
                        Button::new("peers-off")
                            .small()
                            .ghost()
                            .label("Turn off sync")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.store.update(cx, |s, cx| s.set_peer_sync(false, cx))
                            })),
                    ),
            )
            .children(follow)
            .child(self.mailbox_card(cx))
            .children(message)
            .child(
                v_flex()
                    .gap_1()
                    .child(div().text_sm().font_semibold().child("On this network"))
                    .child(peers),
            )
            .children(panel)
            .child(advanced)
            )
            .into_any_element()
    }
}
