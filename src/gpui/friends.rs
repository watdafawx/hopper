//! The Friends panel, docked beside the Library from the title bar's
//! Friends button: friends and the packs they share, the packs this PC
//! shares and who sees each, and the invite and join codes. The work is in
//! `store/friends.rs`; this draws it. The protocol is in
//! `design/friends-sync.md`.

use crate::drag_pick;
use crate::store::{FollowReq, FollowTarget, Store, SyncStatus};
use crate::widgets::{self, Clickable as _};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::dialog::{DialogAction, DialogClose, DialogFooter};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::radio::Radio;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, Selectable as _, Sizable as _, StyledExt as _, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use hopper::groupsync::Kind;
use hopper::instances::Instance;
use hopper::mailbox::{Event, Friend, Group};
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// How long the relay keeps an invite (design/friends-sync.md), for the
/// countdown's bar; the time left itself comes from the relay.
const INVITE_SECS: f32 = 15. * 60.;

/// A friend's pack nobody here follows, unchanged this long, can be hidden.
const STALE_SECS: u64 = 30 * 24 * 3600;

/// What a button says while Hopper can't reach the friends service.
const CANT_CONNECT: &str = hopper::mailbox::CANT_CONNECT;

/// What Invite a friend does.
const INVITE_TIP: &str = "Get a code to send your friend. It works once, for 15 minutes.";

/// The three steps, for someone opening Friends for the first time.
fn how_it_works(cx: &App) -> Div {
    let step = |n: &str, text: &str| {
        h_flex()
            .gap_2()
            .items_start()
            .child(div().flex_none().w(rems(1.)).text_sm().font_semibold().child(n.to_string()))
            .child(div().flex_1().min_w_0().text_sm().text_color(cx.theme().muted_foreground).child(text.to_string()))
    };
    v_flex()
        .w_full()
        .gap_1()
        .child(step("1", "Invite a friend. Hopper gives you a code to send them."))
        .child(step("2", "They paste the code in their Hopper. Now you're friends."))
        .child(step("3", "Share a pack. When you change it, their copy changes too."))
}

/// The rail's width and its tiles' size, in rem. 3 rem (48 px at 100%)
/// leaves the narrowest panel (26 rem) 23 rem for the list; the mock's
/// 56 px rail cut a long name and its chip off sooner.
const RAIL_W: f32 = 3.;
const TILE: f32 = 2.375;
/// A rail tile's fill: `foreground` at this opacity. `muted` is the panel's
/// own colour in light themes, so the tiles vanished there.
const TILE_FILL: f32 = 0.08;

/// A friend sharing more packs than this gets a search box on their page.
const PAGE_SEARCH_FROM: usize = 8;

/// What the panel shows beside the rail: everyone, one friend, one group
/// (or its invitation), or the packs.
#[derive(Clone, PartialEq)]
enum Page {
    Everyone,
    Friend(String),
    Group(String),
    Packs,
}

/// Someone in the Friends list: a friend, or someone who shares a pack
/// through a group without being one. `packs` leaves out what's listed
/// under a group.
struct Person {
    friend: Option<Friend>,
    id: String,
    name: String,
    packs: Vec<Event>,
}

/// What the panel asks the Library to do.
pub enum FriendsEvent {
    /// The ✕: hide the panel.
    Close,
    /// Open this instance's page.
    Show(PathBuf),
    /// Open this CurseForge project's page beside the instance.
    Project(u64),
    /// The share popover opened or closed: the window draws it.
    Popover,
}

impl EventEmitter<FriendsEvent> for FriendsPanel {}

pub struct FriendsPanel {
    store: Entity<Store>,
    panel: widgets::SidePanel,
    /// The friend's invite code, to join them.
    join_code: Entity<InputState>,
    /// The Your packs row whose share popover is open (its instance id).
    share_pop: Option<String>,
    /// Where each Share button was last drawn, by instance id, for its
    /// popover.
    anchors: Rc<RefCell<HashMap<String, Bounds<Pixels>>>>,
    /// The popover's tick boxes, dragged across.
    paint: drag_pick::Paint,
    /// The invite countdown's redraw is running.
    ticking: bool,
    page: Page,
    /// On a friend's page, their packs changed after this are New. Opening
    /// the page marks them seen, so this keeps the time from before.
    page_since: u64,
    /// The header's + popover (invite, join, new group) is open.
    plus_pop: bool,
    /// Where the Invite button was last drawn, for its popover.
    plus_anchor: Rc<RefCell<Option<Bounds<Pixels>>>>,
    /// The popover was opened from "Got a code?": only the join part, under
    /// that link.
    join_only: bool,
    join_anchor: Rc<RefCell<Option<Bounds<Pixels>>>>,
    /// The list's search, and the one on a friend's page.
    search: Entity<InputState>,
    pack_search: Entity<InputState>,
    /// A friend's packs A to Z instead of newest first.
    by_name: bool,
    /// The live invite's reminder above the list was closed.
    invite_hidden: bool,
    /// What the page's search field says it searches.
    pack_search_for: String,
    /// Holds the keyboard on a page, so Escape goes back to the list.
    focus: FocusHandle,
    /// The friend being dragged, so the rail can say where they can go.
    dragging: Option<FriendDrag>,
    /// The drop spots drawn this frame, for the drag's hint.
    spots: Rc<RefCell<Vec<DropSpot>>>,
    /// Take-outs waiting for Undo: (group, friend, token).
    pending_out: Vec<(String, String, u64)>,
    out_token: u64,
    _subs: Vec<Subscription>,
}

impl FriendsPanel {
    pub fn new(store: Entity<Store>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let join_code = cx.new(|cx| InputState::new(window, cx).placeholder("Paste their code"));
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search friends and packs"));
        let pack_search = cx.new(|cx| InputState::new(window, cx).placeholder("Search their packs"));
        let subs = vec![
            cx.observe(&store, |_, _, cx| cx.notify()),
            cx.observe(&search, |_, _, cx| cx.notify()),
            cx.observe(&pack_search, |_, _, cx| cx.notify()),
            cx.subscribe_in(&join_code, window, |this, _, e: &InputEvent, window, cx| {
                if matches!(e, InputEvent::PressEnter { .. }) {
                    this.join(window, cx);
                }
            }),
        ];
        Self {
            store,
            panel: widgets::SidePanel::new(widgets::DOCK),
            join_code,
            share_pop: None,
            anchors: Rc::default(),
            paint: drag_pick::Paint::default(),
            ticking: false,
            page: Page::Everyone,
            page_since: 0,
            plus_pop: false,
            plus_anchor: Rc::default(),
            join_only: false,
            join_anchor: Rc::default(),
            search,
            pack_search,
            by_name: false,
            invite_hidden: false,
            pack_search_for: String::new(),
            focus: cx.focus_handle(),
            dragging: None,
            spots: Rc::default(),
            pending_out: Vec::new(),
            out_token: 0,
            _subs: subs,
        }
    }

    /// The panel's width in rem, for the page beside it.
    pub fn width(&self, window: &Window, cx: &App) -> f32 {
        self.panel.layout(self.store.read(cx).settings(), window).width
    }

    /// Opens the share popover of a pack (instance id), or closes it.
    fn set_pop(&mut self, pop: Option<String>, cx: &mut Context<Self>) {
        if pop.is_some() {
            self.set_plus(false, cx);
        }
        if self.share_pop != pop {
            self.share_pop = pop;
            cx.emit(FriendsEvent::Popover);
        }
        cx.notify();
    }

    /// Opens or closes the header's + popover.
    fn set_plus(&mut self, open: bool, cx: &mut Context<Self>) {
        if open {
            self.set_pop(None, cx);
        } else {
            self.join_only = false;
        }
        if self.plus_pop != open {
            self.plus_pop = open;
            cx.emit(FriendsEvent::Popover);
        }
        cx.notify();
    }

    /// The panel was opened or closed.
    pub fn shown(&mut self, open: bool, cx: &mut Context<Self>) {
        self.set_pop(None, cx);
        self.set_plus(false, cx);
        if open {
            self.store.update(cx, |s, cx| s.mark_friends_seen(cx));
        } else {
            // It opens on the list again.
            self.leave_page(cx);
            self.page = Page::Everyone;
        }
        cx.notify();
    }

    /// Leaving a friend's page: what it showed isn't new any more.
    fn leave_page(&mut self, cx: &mut Context<Self>) {
        if let Page::Friend(id) = &self.page {
            let id = id.clone();
            self.store.update(cx, |s, cx| s.mark_friend_seen(&id, cx));
        }
    }

    /// Shows a page in the Friends tab, or the list.
    fn open(&mut self, page: Page, window: &mut Window, cx: &mut Context<Self>) {
        self.leave_page(cx);
        if let Page::Friend(id) = &page {
            self.page_since = self.store.read(cx).friend_seen(id);
            let id = id.clone();
            self.store.update(cx, |s, cx| s.mark_friend_seen(&id, cx));
        }
        widgets::clear_search(&self.pack_search, window, cx);
        if page != Page::Everyone {
            window.focus(&self.focus, cx);
        }
        self.set_pop(None, cx);
        self.page = page;
        cx.notify();
    }

    fn join(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let code = self.join_code.read(cx).value().trim().to_string();
        if code.is_empty() {
            return;
        }
        self.store.update(cx, |s, cx| s.join_friend(&code, cx));
        // Codes work once, so there's nothing to retry with.
        self.join_code.update(cx, |i, cx| i.set_value("", window, cx));
        self.set_plus(false, cx);
    }

    /// Redraws once a second while an invite counts down.
    fn tick(&mut self, cx: &mut Context<Self>) {
        if self.ticking {
            return;
        }
        self.ticking = true;
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let more = this
                    .update(cx, |this, cx| {
                        cx.notify();
                        let live = this.plain_invite(cx).is_some();
                        this.ticking = live;
                        live
                    })
                    .unwrap_or(false);
                if !more {
                    break;
                }
            }
        })
        .detach();
    }

    /// The live invite made here. One made by a share code's follow part
    /// shows in that dialog instead.
    fn plain_invite(&self, cx: &App) -> Option<crate::store::Invite> {
        self.store.read(cx).mailbox_invite("")
    }

    /// Opens the follow chooser for a friend's pack.
    fn follow(&mut self, e: Event, window: &Window, cx: &mut Context<Self>) {
        // The clicked window, not the active one: a click can reach a
        // window in the background (MCP, or one not yet focused).
        let handle = window.window_handle();
        let store = self.store.clone();
        // Opening a dialog from inside a click handler is too early; wait
        // for the window's next update like the other dialogs.
        cx.defer(move |cx| {
            handle.update(cx, |_, window, cx| open_follow(store, &e, window, cx)).ok();
        });
    }

    fn confirm_remove(&mut self, f: Friend, window: &Window, cx: &mut Context<Self>) {
        let handle = window.window_handle();
        let store = self.store.clone();
        cx.defer(move |cx| {
            handle
                .update(cx, |_, window, cx| {
                    let name = name_or(&f.name, "this friend");
                    window.open_alert_dialog(cx, move |alert, _, _| {
                        let (store, id) = (store.clone(), f.id.clone());
                        alert
                            .title(format!("Remove {name}?"))
                            .description(format!(
                                "{name} stops getting your packs, and you stop getting theirs. Packs you already got from {name} stay on your PC as they are. You can invite {name} again later."
                            ))
                            .confirm()
                            .ok_text("Remove friend")
                            .ok_variant(gpui_kit::component::button::ButtonVariant::Danger)
                            .on_ok(move |_, _, cx| {
                                store.update(cx, |s, cx| s.remove_friend(&id, cx));
                                true
                            })
                    });
                })
                .ok();
        });
    }

    /// The popover over the whole window, under the Share button that
    /// opened it. `None` when none is open. Drawn by the app's root.
    pub fn overlay(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.plus_pop {
            return self.plus_overlay(window, cx);
        }
        let inst_id = self.share_pop.clone()?;
        let anchor = self.anchors.borrow().get(&inst_id).copied()?;
        let s = self.store.read(cx);
        let Some(inst) = s.instances().iter().find(|i| i.id == inst_id).cloned() else {
            return None;
        };
        let theme = cx.theme().clone();
        let friends = s.mailbox_friends().to_vec();
        let takes_back = s.mailbox().and_then(|m| m.takes_back()).unwrap_or(false);
        let down = matches!(s.sync_status(), SyncStatus::Down(_));
        let place = widgets::anchored_popover(anchor, window);
        let me = cx.entity();
        let group: SharedString = "share-pop".into();
        let mut rows: Vec<AnyElement> = Vec::new();
        for (i, f) in friends.iter().enumerate() {
            let on = self.store.read(cx).settings().shares_with(&inst.id, &f.id);
            let set = {
                let (store, inst, fid) = (self.store.clone(), inst.id.clone(), f.id.clone());
                Rc::new(move |v: usize, _: &mut Window, cx: &mut App| {
                    let want = v == 1;
                    if store.read(cx).settings().shares_with(&inst, &fid) != want {
                        store.update(cx, |s, cx| s.set_shared(&inst, &fid, want, cx));
                    }
                })
            };
            let (s1, s2, paint, g2) = (set.clone(), set.clone(), self.paint.clone(), group.clone());
            let name = name_or(&f.name, "A friend");
            let head = self.head(&f.mc_id, &name, rems(1.25), cx);
            let row = drag_pick::paint_row(h_flex().id(("share-row", i)), &self.paint, group.clone(), move |v, w, cx| s1(v, w, cx))
                .gap_2()
                .px_2()
                .py_1()
                .rounded(theme.radius)
                .hover(|s| widgets::row_hover(s, cx))
                .child(
                    drag_pick::paint_option(div(), &self.paint, group.clone(), usize::from(!on), move |v, w, cx| s2(v, w, cx))
                        .child(Checkbox::new(("share-tick", i)).checked(on).disabled(down).on_click({
                            let set = set.clone();
                            move |v: &bool, w, cx| {
                                // The press already set it.
                                if paint.active(&g2) {
                                    return;
                                }
                                set(usize::from(*v), w, cx)
                            }
                        })),
                )
                .child(head)
                .child(div().flex_1().min_w_0().truncate().text_sm().child(name))
                .when(!f.online, |r| r.child(widgets::caption("offline", cx).text_xs()));
            rows.push(row.into_any_element());
        }
        // Groups this PC is in: everyone in one sees the pack.
        let groups: Vec<Group> = self.store.read(cx).mailbox_groups().iter().filter(|g| g.me.joined).cloned().collect();
        let ggroup: SharedString = "share-pop-g".into();
        let mut grows: Vec<AnyElement> = Vec::new();
        for (i, g) in groups.iter().enumerate() {
            let on = self.store.read(cx).settings().shares_with_group(&inst.id, &g.id);
            let set = {
                let (store, inst, gid) = (self.store.clone(), inst.id.clone(), g.id.clone());
                Rc::new(move |v: usize, _: &mut Window, cx: &mut App| {
                    let want = v == 1;
                    if store.read(cx).settings().shares_with_group(&inst, &gid) != want {
                        store.update(cx, |s, cx| s.set_shared_group(&inst, &gid, want, cx));
                    }
                })
            };
            let (s1, s2, paint, g2) = (set.clone(), set.clone(), self.paint.clone(), ggroup.clone());
            let me_id = self.store.read(cx).mailbox_id();
            let n = g.members.iter().filter(|m| m.joined && m.id != me_id).count();
            grows.push(
                drag_pick::paint_row(h_flex().id(("share-grow", i)), &self.paint, ggroup.clone(), move |v, w, cx| s1(v, w, cx))
                    .gap_2()
                    .px_2()
                    .py_1()
                    .rounded(theme.radius)
                    .hover(|s| widgets::row_hover(s, cx))
                    .child(
                        drag_pick::paint_option(div(), &self.paint, ggroup.clone(), usize::from(!on), move |v, w, cx| s2(v, w, cx))
                            .child(Checkbox::new(("share-gtick", i)).checked(on).disabled(down).on_click({
                                let set = set.clone();
                                move |v: &bool, w, cx| {
                                    if paint.active(&g2) {
                                        return;
                                    }
                                    set(usize::from(*v), w, cx)
                                }
                            })),
                    )
                    .child(
                        div()
                            .flex_none()
                            .size(rems(1.25))
                            .rounded_full()
                            .bg(theme.muted)
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(Icon::new(IconName::Users).xsmall().text_color(theme.muted_foreground)),
                    )
                    .child(div().flex_1().min_w_0().truncate().text_sm().child(g.name.clone()))
                    .child(widgets::caption(format!("{n} friend{}", if n == 1 { "" } else { "s" }), cx).text_xs())
                    .into_any_element(),
            );
        }
        // Mods kept only on this PC: how they are here, and Sync again.
        let kept: Vec<AnyElement> = inst
            .only_here
            .iter()
            .enumerate()
            .map(|(i, (key, k))| {
                let a = inst.addons.iter().find(|a| hopper::groupsync::addon_key(a) == *key);
                let name = a.map(|a| a.name.clone()).filter(|n| !n.is_empty()).unwrap_or_else(|| k.name.clone());
                let name = if name.is_empty() { "A mod".to_string() } else { name };
                let here = match a {
                    None => "removed here",
                    Some(a) if !a.enabled => "off here",
                    Some(_) => "on here",
                };
                let icon = a.and_then(|a| self.store.read(cx).image(&a.icon)).map(Path::to_path_buf);
                let (store, root, key) = (self.store.clone(), inst.root.clone(), key.clone());
                let (view, pid) = (me.clone(), a.map_or(0, |a| a.project_id));
                h_flex()
                    .id(("kept-row", i))
                    .gap_2()
                    .px_2()
                    .py_1()
                    .rounded(theme.radius)
                    .hover(|s| widgets::row_hover(s, cx))
                    .child(div().flex_1().min_w_0().child(
                        widgets::mod_link(("kept-name", i), name, icon.as_deref(), pid, move |p, _, cx| view.update(cx, |_, cx| cx.emit(FriendsEvent::Project(p))), cx)
                            .text_sm(),
                    ))
                    .child(widgets::caption(here, cx).text_xs().flex_none())
                    .child(
                        Button::new(("kept-sync", i))
                            .xsmall()
                            .ghost()
                            .label("Sync again")
                            .tooltip("Put it back the way friends have it")
                            .on_click(move |_, window, cx| {
                                crate::addons::confirm_sync_again(store.clone(), root.clone(), vec![key.clone()], window, cx)
                            }),
                    )
                    .into_any_element()
            })
            .collect();
        let paint_end = self.paint.clone();
        let copy = {
            let (store, root) = (self.store.clone(), inst.root.clone());
            move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                crate::dialogs::open_share(store.clone(), root.clone(), window, cx)
            }
        };
        let pop = v_flex()
            .id("share-pop")
            .occlude()
            .gap_1()
            .p_2()
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(theme.border)
            .bg(theme.popover)
            .shadow_lg()
            .on_mouse_up(MouseButton::Left, move |_, _, _| paint_end.end())
            .on_mouse_down_out({
                let me = me.clone();
                move |e: &MouseDownEvent, _, cx| {
                    // The Share button toggles it itself.
                    if anchor.contains(&e.position) {
                        return;
                    }
                    me.update(cx, |t, cx| t.set_pop(None, cx))
                }
            })
            .child(
                div()
                    .flex_none()
                    .px_2()
                    .pt_1()
                    .pb_1()
                    .text_sm()
                    .font_semibold()
                    .truncate()
                    .child(format!("Who gets {}", inst.name)),
            )
            .child(div().px_2().pb_1().child(widgets::fine_print(
                "Ticked friends get this pack and every change you make to it. Only the mod list is sent. The mods download from their own sites.",
                cx,
            )))
            .when(friends.is_empty(), |c| {
                c.child(div().px_2().child(widgets::fine_print("No friends yet. Invite a friend first, then pick who gets this pack.", cx)))
            })
            .when(!friends.is_empty(), |c| c.child(div().px_2().child(widgets::caption("Friends", cx).text_xs())))
            // Only the list scrolls, and only past 16rem. A plain scroll
            // area sizes to its rows; a scrollbar wrapper here reports no
            // height, which squeezed the title and rows (and, on the popover
            // itself, dropped its placement).
            .child(
                v_flex()
                    .id("share-pop-list")
                    .flex_none()
                    .max_h(rems(16.))
                    .overflow_y_scroll()
                    .gap_1()
                    .children(rows)
                    .when(!grows.is_empty(), |c| {
                        c.child(div().px_2().pt_1().child(widgets::caption("Groups", cx).text_xs())).children(grows)
                    }),
            )
            .when(!kept.is_empty(), |c| {
                c.child(
                    v_flex()
                        .flex_none()
                        .gap_1()
                        .mt_1()
                        .pt_2()
                        .border_t_1()
                        .border_color(theme.border)
                        .child(div().px_2().text_sm().font_semibold().child("Only on this PC"))
                        .child(div().px_2().pb_1().child(widgets::fine_print("Friends keep these as they have them.", cx)))
                        .children(kept),
                )
            })
            .child(
                v_flex()
                    .flex_none()
                    .gap_2()
                    .mt_1()
                    .px_2()
                    .pt_2()
                    .border_t_1()
                    .border_color(theme.border)
                    // Nothing to untick yet: the line would talk about people who aren't there.
                    .when(!friends.is_empty(), |c| {
                        c.child(widgets::fine_print(
                            if takes_back {
                                "Ticked friends see this pack in their Friends panel and can follow it. Untick someone and they stop getting your changes. The pack leaves their list, but their copy stays on their PC."
                            } else {
                                "Ticked friends see this pack in their Friends panel and can follow it. Untick someone and they stop getting your changes. Their copy stays on their PC."
                            },
                            cx,
                        ))
                    })
                    .child(
                        h_flex().child(
                            Button::new("share-pop-code")
                                .small()
                                .ghost()
                                .icon(IconName::Copy)
                                .label("Copy a share code")
                                .tooltip("A code anyone can paste in Hopper to install this pack, even if they aren't your friend yet")
                                .on_click(copy),
                        ),
                    ),
            );
        Some(place.apply(pop, 17., 22., window).into_any_element())
    }

    /// The Invite popover: invite a friend, or join with their code.
    fn plus_overlay(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let join_only = self.join_only;
        let anchor = if join_only { *self.join_anchor.borrow() } else { *self.plus_anchor.borrow() }?;
        let theme = cx.theme().clone();
        let down = matches!(self.store.read(cx).sync_status(), SyncStatus::Down(_));
        let place = widgets::anchored_popover(anchor, window);
        let me = cx.entity();
        let invite = (!join_only).then(|| self.invite_block(down, true, cx));
        let join = self.join_block(down, cx);
        let pop = v_flex()
            .id("plus-pop")
            .occlude()
            .gap_2()
            .p_2()
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(theme.border)
            .bg(theme.popover)
            .shadow_lg()
            .on_mouse_down_out(move |e: &MouseDownEvent, _, cx| {
                // The button that opened it toggles it itself.
                if anchor.contains(&e.position) {
                    return;
                }
                me.update(cx, |t, cx| t.set_plus(false, cx))
            })
            .when_some(invite, |p, i| p.child(i).child(or_line(cx)))
            .child(join);
        Some(place.apply(pop, 18.5, 18.5, window).into_any_element())
    }

    /// A friend's face, from their Minecraft account; a plain circle when
    /// they aren't signed in.
    fn head(&self, mc_id: &str, name: &str, size: Rems, cx: &mut Context<Self>) -> AnyElement {
        face(&self.store, mc_id, name, size, cx)
    }

    /// Groups can be made: the relay answers and there's a friend to add.
    fn can_group(&self, cx: &App) -> bool {
        let s = self.store.read(cx);
        matches!(s.sync_status(), SyncStatus::Ok) && !s.mailbox_friends().is_empty()
    }

    /// Opens the group dialog once the click that asked for it is done.
    fn group_dialog(&mut self, mode: GroupMode, pick: Option<String>, window: &Window, cx: &mut Context<Self>) {
        let handle = window.window_handle();
        let store = self.store.clone();
        cx.defer(move |cx| {
            handle.update(cx, |_, window, cx| open_group(store, mode, pick, window, cx)).ok();
        });
    }

    /// Asks before taking someone out of a group or deleting it.
    fn confirm_group(&mut self, title: String, body: String, ok: &'static str, act: Rc<dyn Fn(&mut Store, &mut Context<Store>)>, window: &Window, cx: &mut Context<Self>) {
        let handle = window.window_handle();
        let store = self.store.clone();
        cx.defer(move |cx| {
            handle
                .update(cx, |_, window, cx| {
                    window.open_alert_dialog(cx, move |alert, _, _| {
                        let (store, act) = (store.clone(), act.clone());
                        alert
                            .title(title.clone())
                            .description(body.clone())
                            .confirm()
                            .ok_text(ok)
                            .ok_variant(gpui_kit::component::button::ButtonVariant::Danger)
                            .on_ok(move |_, _, cx| {
                                store.update(cx, |s, cx| act(s, cx));
                                true
                            })
                    });
                })
                .ok();
        });
    }

    /// How the packs this PC follows are doing, in one chip beside the
    /// title: the one that needs the player most. Clicking it opens Packs,
    /// where the card says which pack and why.
    fn sync_chip(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let s = self.store.read(cx);
        if s.sync_status() != SyncStatus::Ok {
            return None;
        }
        let rows = sync_rows(s);
        let top = rows.first()?;
        let theme = cx.theme().clone();
        let need = rows.iter().filter(|r| r.state.rank() <= 1).count();
        let more = |one: String| if need > 1 { format!("{one} {} more need a look.", need - 1) } else { one };
        let (icon, color, label, tip) = match &top.state {
            CopyState::Failed(..) => (
                IconName::TriangleAlert,
                theme.danger,
                "Couldn't update",
                more(top.state.note(&top.who).map(|n| format!("{}: {n}", top.name)).unwrap_or_default()),
            ),
            CopyState::Held(_) => (
                IconName::Info,
                theme.warning,
                "Needs your answer",
                more(top.state.note(&top.who).map(|n| format!("{}: {n}", top.name)).unwrap_or_default()),
            ),
            CopyState::Updating => (IconName::RefreshCw, theme.primary, "Syncing", format!("Getting {}'s changes for {}.", top.who, top.name)),
            CopyState::UpToDate => (
                IconName::CircleCheck,
                theme.success,
                "In sync",
                if rows.len() == 1 { "The pack you follow is up to date.".to_string() } else { format!("All {} packs you follow are up to date.", rows.len()) },
            ),
        };
        // Red text on its own dark tint reads too dim (3.8:1): lift it, as
        // the drop zone does.
        let color = if theme.mode.is_dark() && color == theme.danger { hsla(color.h, color.s, color.l.max(0.68), 1.) } else { color };
        let tip: SharedString = tip.into();
        Some(
            div()
                .id("friends-sync")
                .flex_none()
                .rounded(crate::theme::radius_sm())
                .clickable(cx)
                .tooltip(move |w, cx| gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(w, cx))
                .on_click(cx.listener(|this, _, window, cx| this.open(Page::Packs, window, cx)))
                .child(widgets::chip(color).child(h_flex().gap_1().child(Icon::new(icon).xsmall()).child(label)))
                .into_any_element(),
        )
    }

    fn header(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let chip = self.sync_chip(cx);
        let theme = cx.theme();
        h_flex()
            .flex_none()
            .h(rems(3.))
            .px_3()
            .gap_2()
            .child(div().flex_1().min_w_0().truncate().text_base().font_semibold().child("Friends"))
            .children(chip)
            // The empty page has its own Invite button.
            .when(self.store.read(cx).sync_status() != SyncStatus::Off && !self.nobody(cx), |h| {
                let anchor = self.plus_anchor.clone();
                h.child(
                    div()
                        .relative()
                        .flex_none()
                        .child(
                            Button::new("friends-plus")
                                .outline()
                                .small()
                                .icon(IconName::Plus)
                                .label("Invite")
                                .selected(self.plus_pop)
                                .tooltip("Invite a friend, or join a friend with their code")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    let open = !this.plus_pop;
                                    this.join_only = false;
                                    this.set_plus(open, cx)
                                })),
                        )
                        .child(
                            canvas(move |b, _, _| *anchor.borrow_mut() = Some(b), |_, _, _, _| {})
                                .absolute()
                                .top_0()
                                .left_0()
                                .size_full(),
                        ),
                )
            })
            .child(
                Button::new("friends-close")
                    .ghost()
                    .small()
                    .icon(IconName::Close)
                    .tooltip("Close the Friends panel")
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(FriendsEvent::Close))),
            )
            .border_b_1()
            .border_color(theme.border)
    }

    /// Friends is off: what it is, and a button to turn it on.
    fn off(&self, cx: &mut Context<Self>) -> AnyElement {
        let err = self.store.read(cx).peer_error();
        v_flex()
            .size_full()
            .justify_center()
            .child(
                widgets::empty_state(
                    IconName::Users,
                    "Friends is off",
                    Some("Turn on friends to share packs and play the same ones as your friends.".into()),
                    cx,
                )
                .child(div().px_4().pb_2().child(how_it_works(cx)))
                .child(
                    Button::new("friends-on")
                        .primary()
                        .label("Turn on friends")
                        .on_click(cx.listener(|this, _, _, cx| this.store.update(cx, |s, cx| s.set_peer_sync(true, cx)))),
                )
                .when_some(err, |c, e| c.child(div().text_sm().text_color(cx.theme().danger).child(e))),
            )
            .into_any_element()
    }

    /// The live invite's code and countdown, or the Invite button. In the
    /// + popover (`in_pop`) the popover is the frame, and what the button
    /// does is said under it.
    fn invite_block(&mut self, down: bool, in_pop: bool, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let s = self.store.read(cx);
        if let Some(inv) = self.plain_invite(cx) {
            self.tick(cx);
            let left = inv.until.saturating_duration_since(Instant::now()).as_secs();
            let shown = if inv.code.len() == 8 { format!("{} {}", &inv.code[..4], &inv.code[4..]) } else { inv.code.clone() };
            let copy = inv.code.clone();
            let frac = (left as f32 / INVITE_SECS).clamp(0., 1.);
            return v_flex()
                .gap_2()
                .map(|c| if in_pop { c.px_1().pt_1() } else { c.p_3().rounded(theme.radius_lg).border_1().border_color(theme.border) })
                .child(div().text_sm().font_medium().child("Send this code to your friend"))
                .child(widgets::fine_print("They paste it under \u{201c}Got a code from a friend?\u{201d} in their Friends panel.", cx))
                .child(
                    h_flex()
                        .gap_2()
                        .child(div().flex_1().text_2xl().font_semibold().font_family(theme.mono_font_family.clone()).child(shown))
                        .child(
                            Button::new("invite-copy")
                                .small()
                                .outline()
                                .icon(IconName::Copy)
                                .label("Copy")
                                .tooltip("Copy the code to send it in a chat or message")
                                .on_click(move |_, window, cx| {
                                    cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()));
                                    window.push_notification("Code copied. Send it to your friend.", cx);
                                }),
                        ),
                )
                .child(
                    div()
                        .h(px(4.))
                        .w_full()
                        .rounded_full()
                        .bg(theme.muted)
                        .child(div().h_full().rounded_full().bg(theme.primary).w(relative(frac))),
                )
                .child(widgets::fine_print(
                    format!("Waiting for them to join · Works for one friend · {:02}:{:02} left", left / 60, left % 60),
                    cx,
                ))
                .into_any_element();
        }
        if s.friends_full() {
            return v_flex()
                .gap_1()
                .child(Button::new("invite").w_full().primary().label("Friend list full").disabled(true))
                .child(widgets::fine_print("Your friend list is full. Remove a friend to add someone new.", cx))
                .into_any_element();
        }
        let button = Button::new("invite")
            .w_full()
            .primary()
            .icon(IconName::UserPlus)
            .label("Invite a friend")
            .disabled(down)
            .tooltip(if down { CANT_CONNECT } else { INVITE_TIP })
            .on_click(cx.listener(|this, _, _, cx| {
                this.invite_hidden = false;
                this.store.update(cx, |s, cx| s.make_invite("", cx))
            }));
        if !in_pop {
            return button.into_any_element();
        }
        v_flex()
            .gap_1()
            .child(button)
            .child(div().px_1().child(widgets::fine_print(if down { CANT_CONNECT } else { INVITE_TIP }, cx)))
            .into_any_element()
    }

    fn join_block(&self, down: bool, cx: &mut Context<Self>) -> AnyElement {
        v_flex()
            .gap_1()
            .child(widgets::caption("Got a code from a friend?", cx))
            .child(
                h_flex()
                    .gap_2()
                    .child(div().flex_1().min_w_0().child(Input::new(&self.join_code).small()))
                    .child(
                        Button::new("join")
                            .small()
                            .outline()
                            .label("Join")
                            .disabled(down)
                            .tooltip(if down { CANT_CONNECT } else { "Become friends with whoever sent the code" })
                            .on_click(cx.listener(|this, _, window, cx| this.join(window, cx))),
                    ),
            )
            .into_any_element()
    }

    /// "Your friends can't see your packs yet", once, after the first friend.
    fn prompt(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        v_flex()
            .gap_1()
            .p_3()
            .rounded(theme.radius_lg)
            .bg(theme.primary.opacity(widgets::alpha::TINT))
            .child(div().text_sm().font_semibold().child("Your friends can't get your packs yet"))
            // Muted grey drops under 4.5:1 on the orange tint in light themes.
            .child(div().text_xs().text_color(theme.foreground.opacity(0.8)).child("Every pack starts private. Choose which friends get each one."))
            .child(
                h_flex()
                    .gap_2()
                    .pt_1()
                    .child(Button::new("prompt-choose").small().outline().label("Choose packs").on_click(cx.listener(|this, _, _, cx| {
                        this.page = Page::Packs;
                        this.store.update(cx, |s, cx| s.dismiss_share_prompt(cx));
                    })))
                    .child(
                        Button::new("prompt-not-now")
                            .small()
                            .ghost()
                            .label("Not now")
                            .on_click(cx.listener(|this, _, _, cx| this.store.update(cx, |s, cx| s.dismiss_share_prompt(cx)))),
                    ),
            )
            .into_any_element()
    }

    /// Everyone in the list: friends (online first, then the most recently
    /// seen), then anyone who shares a pack outside a group without being a
    /// friend, so their pack doesn't vanish.
    fn people(&self, recipes: &[Event], groups: &[Group], down: bool, cx: &App) -> Vec<Person> {
        let s = self.store.read(cx);
        // A pack that came only through groups shown in the list is on the
        // group's page.
        let in_groups = |e: &Event| {
            !e.direct && !e.groups.is_empty() && e.groups.iter().all(|g| groups.iter().any(|x| &x.id == g && x.me.joined))
        };
        let theirs: Vec<&Event> = recipes.iter().filter(|e| !in_groups(e)).collect();
        let mut friends: Vec<Friend> = s.mailbox_friends().to_vec();
        friends.sort_by_key(|f| (std::cmp::Reverse(f.online && !down), std::cmp::Reverse(f.last_seen)));
        let mut people: Vec<Person> = friends
            .into_iter()
            .map(|f| Person { id: f.id.clone(), name: name_or(&f.name, "A friend"), friend: Some(f), packs: Vec::new() })
            .collect();
        for e in &theirs {
            if !people.iter().any(|p| p.id == e.from) {
                people.push(Person { friend: None, id: e.from.clone(), name: name_or(&e.from_name, "A friend"), packs: Vec::new() });
            }
        }
        for p in &mut people {
            p.packs = theirs.iter().filter(|e| e.from == p.id).map(|e| (*e).clone()).collect();
        }
        people
    }

    /// How many of this PC's packs `id` gets.
    fn gets(&self, id: &str, cx: &App) -> usize {
        let s = self.store.read(cx);
        s.instances().iter().filter(|i| !i.id.is_empty() && s.settings().shares_with(&i.id, id)).count()
    }

    /// A friend's ⋯: share with them, groups, remove.
    fn friend_menu(&self, id: SharedString, f: Friend, name: &str, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let drag = self.drag_of(&f.id, name, None);
        let add_to = self.group_choices(&drag, false, cx);
        let can_group = self.can_group(cx);
        let view = cx.entity();
        let first = name.to_string();
        Button::new(id)
            .ghost()
            .xsmall()
            .icon(IconName::Ellipsis)
            .tooltip("More")
            .dropdown_menu_with_anchor(Anchor::TopRight, move |menu, window, cx| {
                let (v1, v2, f2) = (view.clone(), view.clone(), f.clone());
                let mut menu = menu.item(PopupMenuItem::new(format!("Choose packs to share with {first}")).icon(IconName::Share2).on_click(move |_, _, cx| {
                    v1.update(cx, |t, cx| {
                        t.page = Page::Packs;
                        cx.notify();
                    })
                }));
                // Every group, the ones that can't take them saying why, then a new one.
                if !add_to.is_empty() {
                    menu = group_submenu(menu, "Add to group", &view, &add_to, &drag, true, window, cx);
                }
                if can_group {
                    let (v, fid) = (view.clone(), f.id.clone());
                    menu = menu.item(PopupMenuItem::new(format!("New group with {first}…")).icon(IconName::Plus).on_click(move |_, window, cx| {
                        let fid = fid.clone();
                        v.update(cx, |t, cx| t.group_dialog(GroupMode::New, Some(fid), window, cx))
                    }));
                }
                menu.separator().item(
                    PopupMenuItem::new("Remove friend…")
                        .icon(IconName::UserMinus)
                        .on_click(move |_, window, cx| v2.update(cx, |t, cx| t.confirm_remove(f2.clone(), window, cx))),
                )
            })
    }

    /// A friend's row in the list: face, name, whether they're online and
    /// how many packs they share, what's new. The row opens their page.
    fn friend_row(&self, ix: usize, p: &Person, words: &[String], down: bool, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let packs = match p.packs.len() {
            0 => "no packs yet".to_string(),
            1 => "1 pack".to_string(),
            n => format!("{n} packs"),
        };
        let sub = format!("{} · {packs}", status(p.friend.as_ref(), down));
        // As their page's New section counts: changed since the page was
        // last open, and not followed (those show under Following).
        let s = self.store.read(cx);
        let seen = s.friend_seen(&p.id);
        let news = p.packs.iter().filter(|e| e.at > seen && s.followers_of(&e.from, &e.inst).is_empty()).count();
        let online = !down && p.friend.as_ref().is_some_and(|f| f.online);
        let mc_id = p.friend.as_ref().map(|f| f.mc_id.clone()).unwrap_or_default();
        let head = self.head(&mc_id, &p.name, rems(1.75), cx);
        let row_id = SharedString::from(format!("friend:{ix}"));
        let menu = p.friend.clone().map(|f| {
            let m = self.friend_menu(format!("friend-menu:{ix}").into(), f, &p.name, cx);
            widgets::reveal_on_hover(row_id.clone(), SharedString::from(format!("friend-menu-at:{ix}")), m, window, cx)
        });
        let id = p.id.clone();
        let drag = p.friend.is_some().then(|| self.drag_of(&p.id, &p.name, None));
        let row = h_flex()
            .id(row_id.clone())
            .group(row_id)
            .gap_2()
            .px_3()
            .py_1p5()
            .clickable(cx)
            .hover(|s| widgets::row_hover(s, cx));
        let row = match drag {
            Some(d) => self.start_drag(row, d, cx),
            None => row,
        };
        row.on_click(cx.listener(move |this, _, window, cx| this.open(Page::Friend(id.clone()), window, cx)))
            .child(with_dot(head, online, cx))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(div().text_sm().font_medium().truncate().child(widgets::marked(p.name.clone(), words, cx)))
                    .child(widgets::caption(sub, cx).text_xs().truncate()),
            )
            .when(news > 0, |r| r.child(widgets::chip(chip_ink(theme.primary, cx)).child(format!("{news} new"))))
            .children(menu)
            .child(Icon::new(IconName::ChevronRight).small().flex_none().text_color(theme.muted_foreground))
            .into_any_element()
    }

    /// The pages' top: back to the list, then whose page it is.
    fn page_top(&self, who: impl IntoElement, cx: &mut Context<Self>) -> AnyElement {
        v_flex()
            .flex_none()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                h_flex().px_1p5().pt_1p5().child(
                    Button::new("friends-back")
                        .ghost()
                        .small()
                        .icon(IconName::ChevronLeft)
                        .label("Everyone")
                        .on_click(cx.listener(|this, _, window, cx| this.open(Page::Everyone, window, cx))),
                ),
            )
            .child(who)
            .into_any_element()
    }

    /// One friend's page: who they are, then their packs: new ones, the
    /// ones this PC follows, the rest.
    fn friend_page(&mut self, p: &Person, down: bool, window: &mut Window, cx: &mut Context<Self>) -> (Option<AnyElement>, AnyElement) {
        let theme = cx.theme().clone();
        let n = p.packs.len();
        let mut sub = status(p.friend.as_ref(), down);
        if p.friend.is_some() {
            sub += &match self.gets(&p.id, cx) {
                0 => " · gets none of your packs".to_string(),
                g => format!(" · gets {g} of your packs"),
            };
        }
        sub += &match n {
            0 => String::new(),
            1 => " · 1 pack for you".to_string(),
            n => format!(" · {n} packs for you"),
        };
        let online = !down && p.friend.as_ref().is_some_and(|f| f.online);
        let mc_id = p.friend.as_ref().map(|f| f.mc_id.clone()).unwrap_or_default();
        let head = self.head(&mc_id, &p.name, rems(2.5), cx);
        let menu = p.friend.clone().map(|f| self.friend_menu("friend-page-menu".into(), f, &p.name, cx));
        let who = h_flex()
            .gap_2()
            .px_3()
            .pt_1p5()
            .pb_2p5()
            .child(with_dot(head, online, cx))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(div().text_base().font_semibold().truncate().child(p.name.clone()))
                    .child(widgets::caption(sub, cx).text_xs().truncate()),
            )
            .children(menu);
        let top = self.page_top(who, cx);

        let placeholder = format!("Search {}'s packs", p.name);
        if self.pack_search_for != placeholder {
            self.pack_search_for = placeholder.clone();
            self.pack_search.update(cx, |i, cx| i.set_placeholder(placeholder, window, cx));
        }
        let q = self.pack_search.read(cx).value().to_string();
        let words = widgets::search_words(&q);
        let s = self.store.read(cx);
        let mut packs: Vec<Event> = p.packs.iter().filter(|e| hits(&pack_name(e), &words)).cloned().collect();
        if self.by_name {
            packs.sort_by_key(|e| pack_name(e).to_lowercase());
        } else {
            packs.sort_by_key(|e| std::cmp::Reverse(e.at));
        }
        let (followed, rest): (Vec<Event>, Vec<Event>) = packs.into_iter().partition(|e| !s.followers_of(&e.from, &e.inst).is_empty());
        let since = self.page_since;
        let (news, rest): (Vec<Event>, Vec<Event>) = rest.into_iter().partition(|e| e.at > since);
        let rows = |this: &Self, part: &str, list: &[Event], new: bool, window: &mut Window, cx: &mut Context<Self>| -> Vec<AnyElement> {
            list.iter().enumerate().map(|(i, e)| this.their_pack(&format!("pg:{part}:{i}"), e, None, new, down, window, cx)).collect()
        };
        let new_rows = rows(self, "new", &news, true, window, cx);
        let followed_rows = rows(self, "follow", &followed, false, window, cx);
        let rest_rows = rows(self, "rest", &rest, false, window, cx);
        let by_name = self.by_name;
        let view = cx.entity();
        let sort = Button::new("friend-sort")
            .ghost()
            .xsmall()
            .label(if by_name { "A to Z" } else { "Newest first" })
            .dropdown_caret(true)
            .dropdown_menu_with_anchor(Anchor::TopRight, move |menu, _, _| {
                let (v1, v2) = (view.clone(), view.clone());
                menu.item(PopupMenuItem::new("Newest first").checked(!by_name).on_click(move |_, _, cx| {
                    v1.update(cx, |t, cx| {
                        t.by_name = false;
                        cx.notify();
                    })
                }))
                .item(PopupMenuItem::new("A to Z").checked(by_name).on_click(move |_, _, cx| {
                    v2.update(cx, |t, cx| {
                        t.by_name = true;
                        cx.notify();
                    })
                }))
            });
        let nothing = !words.is_empty() && new_rows.is_empty() && followed_rows.is_empty() && rest_rows.is_empty();
        let body = v_flex()
            .pb_2()
            .when(n > PAGE_SEARCH_FROM, |c| c.child(search_box(&self.pack_search)))
            .when(!new_rows.is_empty(), |c| c.child(cap("New", Some(new_rows.len()), None, cx)).children(new_rows))
            .when(!followed_rows.is_empty(), |c| c.child(cap("You follow", Some(followed_rows.len()), None, cx)).children(followed_rows))
            .when(!rest_rows.is_empty(), |c| {
                c.child(cap("Their packs", Some(rest_rows.len()), Some(sort.into_any_element()), cx)).children(rest_rows)
            })
            .when(n == 0, |c| {
                c.child(div().px_3().py_3().child(widgets::fine_print(format!("{} hasn't shared a pack with you yet.", p.name), cx)))
            })
            .when(nothing, |c| c.child(div().px_3().py_3().child(widgets::fine_print(format!("Nothing matches \u{201c}{q}\u{201d}."), cx))))
            .into_any_element();
        let _ = theme;
        (Some(top), body)
    }

    /// A pack a friend shares: Following (into which instance), Update when
    /// a new version waits, or Follow on hover and keyboard focus. `by`:
    /// who shares it, where that isn't said around it. `new`: changed since
    /// their page was last open.
    #[allow(clippy::too_many_arguments)]
    fn their_pack(&self, key: &str, e: &Event, by: Option<String>, new: bool, down: bool, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let s = self.store.read(cx);
        // Each follow here: the instance, its name, and how its copy is
        // doing.
        let follows: Vec<(PathBuf, String, CopyState)> = s
            .followers_of(&e.from, &e.inst)
            .into_iter()
            .map(|f| {
                let root = PathBuf::from(&f.root);
                let local = s.instance(&root).map(|i| i.name.clone()).unwrap_or_else(|| f.name.clone());
                let state = copy_state(s, f, &root);
                (root, local, state)
            })
            .collect();
        let (icon, project) = pack_look(s, &e.code);
        let pack = pack_name(e);
        let sender = name_or(&e.from_name, "your friend");
        // The copy that needs a look first: one that failed, then one with
        // mods to ask about, then one updating.
        let shown = follows.iter().min_by_key(|f| f.2.rank()).cloned();
        // A group pack kept both ways gets everyone's changes, not just the
        // sender's.
        let who = match &shown {
            Some((root, ..)) if s.group_copy(root).is_some() => "the group".to_string(),
            _ => sender.clone(),
        };
        let sub = match &shown {
            Some((_, local, CopyState::Updating)) => format!("Getting {who}'s changes · your copy: {local}"),
            // The note under it says what's wrong: not "up to date".
            Some((_, local, CopyState::Failed(..) | CopyState::Held(_))) => format!("Your copy: {local}"),
            Some((_, local, _)) => format!("Your copy: {local} · up to date"),
            None if e.at > 0 => format!("Updated {}", ago(e.at)),
            None => String::new(),
        };
        let waiting = match &shown {
            Some((root, _, CopyState::Held(_))) => Some(root.clone()),
            _ => None,
        };
        let copy_note = shown.as_ref().and_then(|f| f.2.note(&who));
        let changed = shown.as_ref().and_then(|f| changed_by(s, &f.0, &s.mailbox_id()));
        let sub = match by {
            Some(by) if sub.is_empty() => format!("From {by}"),
            Some(by) => format!("From {by} · {}", sub[..1].to_lowercase() + &sub[1..]),
            None => sub,
        };
        let stale = follows.is_empty() && e.at > 0 && now().saturating_sub(e.at) > STALE_SECS;
        let row_id = SharedString::from(format!("their:{key}"));
        let view = cx.entity();
        let name_el = widgets::mod_link(
            SharedString::from(format!("their-name:{key}")),
            pack,
            icon.as_deref(),
            project,
            move |p, _, cx| view.update(cx, |_, cx| cx.emit(FriendsEvent::Project(p))),
            cx,
        )
        .text_sm();
        let action: AnyElement = match (&waiting, follows.is_empty()) {
            (Some(root), _) => self.update_button(format!("their-update:{key}").into(), e, root.clone(), down, cx).into_any_element(),
            (None, false) => div()
                .id(SharedString::from(format!("following:{key}")))
                .flex_none()
                .tooltip(|w, cx| gpui_kit::component::tooltip::Tooltip::new("You follow this pack. Your copy updates when they change theirs.").build(w, cx))
                .child(widgets::chip(chip_ink(theme.success, cx)).child("Following"))
                .into_any_element(),
            (None, true) => {
                let e = e.clone();
                let b = Button::new(SharedString::from(format!("follow:{key}")))
                    .xsmall()
                    .outline()
                    .label("Follow")
                    .disabled(down)
                    .tooltip(if down { CANT_CONNECT } else { "Follow: your copy updates when they change theirs." })
                    .on_click(cx.listener(move |this, _, window, cx| this.follow(e.clone(), window, cx)));
                widgets::reveal_on_hover(row_id.clone(), SharedString::from(format!("follow-at:{key}")), b, window, cx).into_any_element()
            }
        };
        let hide = stale.then(|| {
            let e = e.clone();
            let b = Button::new(SharedString::from(format!("hide:{key}")))
                .xsmall()
                .ghost()
                .icon(IconName::Close)
                .tooltip("Hide this pack until they change it")
                .on_click(cx.listener(move |this, _, _, cx| this.store.update(cx, |s, cx| s.hide_friend_pack(&e, cx))));
            widgets::reveal_on_hover(row_id.clone(), SharedString::from(format!("hide-at:{key}")), b, window, cx)
        });
        let missing = e.left_out.len();
        h_flex()
            .id(row_id.clone())
            .group(row_id)
            .relative()
            .gap_2()
            .px_3()
            .py_1()
            .hover(|s| widgets::row_hover(s, cx))
            // New since their page was open: a dot in the margin, on the name's line.
            .when(new, |r| {
                r.child(div().absolute().left(px(4.)).top(rems(0.875)).mt(px(-3.)).size(px(6.)).rounded_full().bg(theme.primary))
            })
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(name_el)
                    .when(!sub.is_empty(), |c| {
                        c.child(
                            widgets::caption(sub, cx)
                                .text_xs()
                                .truncate()
                                .pl(rems(1.625))
                                .when(waiting.is_some(), |d| d.text_color(theme.foreground)),
                        )
                    })
                    .when_some(changed, |c, t| c.child(widgets::caption(t, cx).text_xs().truncate().pl(rems(1.625))))
                    .when_some(copy_note, |c, t| c.child(warn_line(t, cx)))
                    .when(missing > 0, |c| {
                        c.child(warn_line(
                            format!(
                                "{missing} file{} can't be downloaded from any site. Ask {sender} to send {}.",
                                if missing == 1 { "" } else { "s" },
                                if missing == 1 { "it" } else { "them" }
                            ),
                            cx,
                        ))
                    }),
            )
            .child(action)
            .children(hide)
            .into_any_element()
    }

    /// Update: brings a followed copy to the friend's newest version, after
    /// the same questions following asks.
    fn update_button(&self, id: SharedString, e: &Event, root: PathBuf, down: bool, cx: &mut Context<Self>) -> Button {
        let e = e.clone();
        Button::new(id)
            .xsmall()
            .outline()
            .icon(IconName::Download)
            .label("Update")
            .disabled(down)
            .tooltip(if down { CANT_CONNECT } else { "Asks about the mods from GitHub and other links, then adds the ones you allow" })
            .on_click(cx.listener(move |this, _, window, cx| {
                let (handle, store, req, root) = (window.window_handle(), this.store.clone(), FollowReq::of(&e), root.clone());
                // After the click, like the other dialogs it may open.
                cx.defer(move |cx| {
                    handle
                        .update(cx, |_, window, cx| crate::store::follow_pack(&store, req, FollowTarget::Existing(root), window, cx))
                        .ok();
                });
            }))
    }

    /// The packs this PC follows, from everyone, a new version waiting
    /// first: the list's top answers "has my pack changed?".
    fn following(&self, recipes: &[Event], words: &[String], down: bool, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let s = self.store.read(cx);
        let mut list: Vec<(Event, PathBuf, CopyState)> = recipes
            .iter()
            .filter(|e| hits(&pack_name(e), words))
            .filter_map(|e| {
                let f = s.followers_of(&e.from, &e.inst).into_iter().find(|f| f.ended.is_empty())?;
                let root = PathBuf::from(&f.root);
                let state = copy_state(s, f, &root);
                Some((e.clone(), root, state))
            })
            .collect();
        list.sort_by_key(|(e, _, state)| (state.rank(), std::cmp::Reverse(e.at)));
        list.iter()
            .enumerate()
            .map(|(i, (e, root, state))| {
                let theme = cx.theme().clone();
                let s = self.store.read(cx);
                let (icon, project) = pack_look(s, &e.code);
                let sender = name_or(&e.from_name, "a friend");
                let who = if s.group_copy(root).is_some() { "the group".to_string() } else { sender.clone() };
                let sub = match state {
                    CopyState::Updating => format!("Getting {who}'s changes · {}", ago(e.at)),
                    CopyState::Failed(..) => format!("From {sender} · changed {}", ago(e.at)),
                    CopyState::Held(_) => format!("From {sender}"),
                    CopyState::UpToDate => format!("From {sender} · up to date"),
                };
                let kept = s.instance(root).map_or(0, |i| i.only_here.len());
                let sub = if kept > 0 { format!("{sub} · {kept} only on this PC") } else { sub };
                let note = state.note(&who);
                let changed = changed_by(s, root, &s.mailbox_id());
                let waiting = matches!(state, CopyState::Held(_));
                let view = cx.entity();
                h_flex()
                    .id(("followed", i))
                    .gap_2()
                    .px_3()
                    .py_1()
                    .hover(|s| widgets::row_hover(s, cx))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(
                                widgets::mod_link(
                                    ("followed-name", i),
                                    pack_name(e),
                                    icon.as_deref(),
                                    project,
                                    move |p, _, cx| view.update(cx, |_, cx| cx.emit(FriendsEvent::Project(p))),
                                    cx,
                                )
                                .text_sm(),
                            )
                            .child(
                                widgets::caption(sub, cx)
                                    .text_xs()
                                    .truncate()
                                    .pl(rems(1.625))
                                    .when(waiting, |d| d.text_color(theme.foreground)),
                            )
                            .when_some(changed, |c, t| c.child(widgets::caption(t, cx).text_xs().truncate().pl(rems(1.625))))
                            .when_some(note, |c, t| c.child(warn_line(t, cx))),
                    )
                    .when(waiting, |r| r.child(self.update_button(format!("followed-update:{i}").into(), e, root.clone(), down, cx)))
                    .into_any_element()
            })
            .collect()
    }

    /// Follows that get nothing more: the friend removed this PC, or the
    /// pack is gone. (root, their pack, the copy here, whose it was)
    fn ended_list(&self, friends: &[Friend], recipes: &[Event], cx: &App) -> Vec<(PathBuf, String, String, String)> {
        let s = self.store.read(cx);
        s.settings()
            .followed_recipes
            .iter()
            .filter(|f| !f.ended.is_empty())
            .filter(|f| !recipes.iter().any(|e| f.matches(&e.from, &e.inst, "")))
            .map(|f| {
                let root = PathBuf::from(&f.root);
                let local = s.instance(&root).map(|i| i.name.clone()).unwrap_or_else(|| f.name.clone());
                let who = if friends.iter().any(|x| x.id == f.peer_id) {
                    format!("{}'s {}", name_or(&f.peer_name, "A friend"), f.name)
                } else {
                    format!("{} · not your friend any more", name_or(&f.peer_name, "A friend"))
                };
                (root, f.name.clone(), local, who)
            })
            .collect()
    }

    /// Someone asked this PC into a group: join or decline.
    fn invitation(&self, gi: usize, g: &Group, down: bool, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let owner = g.members.iter().find(|m| m.id == g.owner).map(|m| name_or(&m.name, "A friend")).unwrap_or_else(|| "A friend".into());
        let (j, d) = (g.id.clone(), g.id.clone());
        h_flex()
            .id(("group-invite", gi))
            .gap_2()
            .px_3()
            .py_1p5()
            .mx_2()
            .my_0p5()
            .rounded(theme.radius)
            .bg(theme.primary.opacity(widgets::alpha::TINT))
            .child(
                div()
                    .flex_none()
                    .size(rems(1.75))
                    .rounded_full()
                    .bg(theme.muted)
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(Icon::new(IconName::Users).xsmall().text_color(theme.foreground)),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(div().text_sm().font_medium().truncate().child(g.name.clone()))
                    .child(div().text_xs().text_color(theme.foreground.opacity(0.8)).child(format!("{owner} asked you to join their group"))),
            )
            .child(
                Button::new(("group-join", gi))
                    .xsmall()
                    .primary()
                    .label("Join")
                    .disabled(down)
                    .tooltip(if down { CANT_CONNECT } else { "Join the group and get the packs it shares" })
                    .on_click(cx.listener(move |this, _, _, cx| this.store.update(cx, |s, cx| s.accept_group(&j, cx)))),
            )
            .child(
                Button::new(("group-decline", gi))
                    .xsmall()
                    .ghost()
                    .label("No thanks")
                    .disabled(down)
                    .tooltip("Say no. They won't be told why.")
                    .on_click(cx.listener(move |this, _, _, cx| this.store.update(cx, |s, cx| s.leave_group(&d, cx)))),
            )
            .into_any_element()
    }

    /// A group's faces (up to three) and its line: who's in it, its packs,
    /// how many of this PC's packs it gets.
    fn group_look(&self, g: &Group, recipes: &[Event], size: Rems, cx: &mut Context<Self>) -> (Div, String) {
        let s = self.store.read(cx);
        let me = s.mailbox_id();
        // Someone being taken out (Undo still open) already counts as gone.
        let others: Vec<_> = g.members.iter().filter(|m| m.id != me && !self.leaving(&g.id, &m.id)).cloned().collect();
        let in_it = others.iter().filter(|m| m.joined).count();
        let asked = others.len() - in_it;
        let mine = s.instances().iter().filter(|i| !i.id.is_empty() && s.settings().shares_with_group(&i.id, &g.id)).count();
        let theirs = recipes.iter().filter(|e| e.from != me && e.groups.contains(&g.id)).count();
        let mut sub = format!("{in_it} friend{}", if in_it == 1 { "" } else { "s" });
        if asked > 0 {
            sub += &format!(" · {asked} not joined yet");
        }
        if theirs > 0 {
            sub += &format!(" · {theirs} pack{}", if theirs == 1 { "" } else { "s" });
        }
        if mine > 0 {
            sub += &format!(" · gets {mine} of your packs");
        }
        let faces: Vec<AnyElement> = others.iter().take(3).map(|m| face(&self.store, &m.mc_id, &m.name, size, cx)).collect();
        (widgets::face_stack(faces, cx.theme().sidebar, size, cx.theme().radius), sub)
    }

    /// A group's ⋯: rename, add friends, delete (its maker), or leave.
    fn group_menu(&self, g: &Group, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let s = self.store.read(cx);
        let mine = g.owner == s.mailbox_id();
        let can_add = mine && s.mailbox_friends().iter().any(|f| !g.members.iter().any(|m| m.id == f.id));
        let (view, g2) = (cx.entity(), g.clone());
        Button::new("group-menu")
            .ghost()
            .xsmall()
            .icon(IconName::Ellipsis)
            .tooltip("More")
            .dropdown_menu_with_anchor(Anchor::TopRight, move |menu, _, _| {
                let (v1, v2, v3, v4) = (view.clone(), view.clone(), view.clone(), view.clone());
                let (g1, g3, g4) = (g2.clone(), g2.clone(), g2.clone());
                let menu = if mine {
                    menu.item(PopupMenuItem::new("Rename…").icon(IconName::Pencil).on_click(move |_, window, cx| {
                        let id = g1.id.clone();
                        v1.update(cx, |t, cx| t.group_dialog(GroupMode::Rename(id), None, window, cx))
                    }))
                    .when(can_add, |m| {
                        let g = g2.clone();
                        m.item(PopupMenuItem::new("Add friends…").icon(IconName::UserPlus).on_click(move |_, window, cx| {
                            let id = g.id.clone();
                            v2.update(cx, |t, cx| t.group_dialog(GroupMode::Add(id), None, window, cx))
                        }))
                    })
                    .separator()
                    .item(PopupMenuItem::new("Delete group…").icon(IconName::Trash).on_click(move |_, window, cx| {
                        let (id, name) = (g3.id.clone(), g3.name.clone());
                        v3.update(cx, |t, cx| {
                            t.confirm_group(
                                format!("Delete {name}?"),
                                "The group's packs stop getting changes for everyone in it. Everyone keeps their copies.".into(),
                                "Delete group",
                                Rc::new(move |s, cx| s.delete_group(&id, cx)),
                                window,
                                cx,
                            )
                        })
                    }))
                } else {
                    menu
                };
                // The owner deletes the group instead (the service won't let
                // them leave it).
                if mine {
                    return menu;
                }
                menu.item(PopupMenuItem::new("Leave group").icon(IconName::LogOut).on_click(move |_, window, cx| {
                    let (id, name) = (g4.id.clone(), g4.name.clone());
                    v4.update(cx, |t, cx| {
                        t.confirm_group(
                            format!("Leave {name}?"),
                            "Packs you got only from this group stop getting changes. Your copies stay on your PC, listed under \u{201c}These won't change any more\u{201d}.".into(),
                            "Leave group",
                            Rc::new(move |s, cx| s.leave_group(&id, cx)),
                            window,
                            cx,
                        )
                    })
                }))
            })
    }

    /// A group's page: an invitation's answer first; then "Keep my packs
    /// the same as the group", the packs in it, and the people.
    fn group_page(&mut self, g: &Group, recipes: &[Event], down: bool, window: &mut Window, cx: &mut Context<Self>) -> (Option<AnyElement>, AnyElement) {
        let theme = cx.theme().clone();
        let (_, sub) = self.group_look(g, recipes, rems(1.5), cx);
        let menu = g.me.joined.then(|| self.group_menu(g, cx));
        let top = h_flex()
            .flex_none()
            .gap_2()
            .pl_3()
            .pr_2()
            .py_1p5()
            .border_b_1()
            .border_color(theme.border)
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(div().text_sm().font_semibold().truncate().child(g.name.clone()))
                    .child(widgets::caption(sub, cx).text_xs().truncate()),
            )
            .children(menu)
            .into_any_element();
        let s = self.store.read(cx);
        let me = s.mailbox_id();
        let mine = g.owner == me;
        let owner_name = g.members.iter().find(|m| m.id == g.owner).map(|m| name_or(&m.name, "A friend")).unwrap_or_else(|| "A friend".into());
        let my_packs: Vec<(PathBuf, String)> = s
            .instances()
            .iter()
            .filter(|i| !i.id.is_empty() && s.settings().shares_with_group(&i.id, &g.id))
            .map(|i| (i.root.clone(), i.name.clone()))
            .collect();
        let can_add = mine && s.mailbox_friends().iter().any(|f| !g.members.iter().any(|m| m.id == f.id));
        // You first, then whoever made it, then the rest.
        let mut members: Vec<_> = g.members.iter().filter(|m| !self.leaving(&g.id, &m.id)).cloned().collect();
        members.sort_by_key(|m| (m.id != me, m.id != g.owner));
        let member_rows: Vec<AnyElement> = members
            .iter()
            .enumerate()
            .map(|(mi, m)| {
                let you = m.id == me;
                let name = if you { "You".to_string() } else { name_or(&m.name, "A friend") };
                let status = if !m.joined {
                    String::new()
                } else if you {
                    String::new()
                } else if m.online && !down {
                    "Online".to_string()
                } else if m.last_seen > 0 {
                    format!("Last online {}", ago(m.last_seen))
                } else {
                    "Offline".to_string()
                };
                let status = match (m.id == g.owner, status.is_empty()) {
                    (true, true) => "Made the group".to_string(),
                    (true, false) => format!("Made the group · {status}"),
                    _ => status,
                };
                let f = face(&self.store, &m.mc_id, &m.name, rems(1.25), cx);
                let row_id = SharedString::from(format!("gm:{mi}"));
                // Dragged off a group this PC made, they move; off another
                // group, they're added where they land.
                let drag = (!you).then(|| self.drag_of(&m.id, &name, mine.then(|| g.id.clone())));
                let menu = drag.clone().map(|d| {
                    let b = self.member_menu(SharedString::from(format!("gm-menu:{mi}")), g, d, down, cx);
                    widgets::reveal_on_hover(row_id.clone(), SharedString::from(format!("gm-menu-at:{mi}")), b, window, cx)
                });
                let row = h_flex()
                    .id(row_id.clone())
                    .group(row_id.clone())
                    .gap_2()
                    .px_3()
                    .py_1()
                    .hover(|s| widgets::row_hover(s, cx));
                let row = match drag {
                    Some(d) if !down => self.start_drag(row, d, cx),
                    _ => row,
                };
                row.child(f)
                    .child(div().flex_1().min_w_0().truncate().text_sm().child(name))
                    .when(!m.joined, |r| r.child(widgets::chip(theme.muted_foreground).child("Invited")))
                    .when(!status.is_empty(), |r| r.child(widgets::caption(status, cx).text_xs().flex_none()))
                    .children(menu)
                    .into_any_element()
            })
            .collect();
        // Packs others share with the group, as the friends' pack rows.
        let shared: Vec<Event> = recipes.iter().filter(|e| e.from != me && e.groups.contains(&g.id)).cloned().collect();
        let pack_rows: Vec<AnyElement> = shared
            .iter()
            .enumerate()
            .map(|(pi, e)| self.their_pack(&format!("g:{pi}"), e, Some(name_or(&e.from_name, "a friend")), false, down, window, cx))
            .collect();
        let in_step = g.me.in_step;
        let step_id = g.id.clone();
        let my_rows: Vec<AnyElement> = my_packs
            .iter()
            .enumerate()
            .map(|(pi, (root, name))| {
                let root = root.clone();
                h_flex()
                    .id(SharedString::from(format!("gp-mine:{pi}")))
                    .gap_2()
                    .px_3()
                    .py_1()
                    .hover(|s| widgets::row_hover(s, cx))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .id(SharedString::from(format!("gp-mine-name:{pi}")))
                                    .truncate()
                                    .text_sm()
                                    .clickable(cx)
                                    .hover(|s| s.text_color(crate::theme::ink(theme.primary)))
                                    .child(name.clone())
                                    .on_click(cx.listener(move |_, _, _, cx| cx.emit(FriendsEvent::Show(root.clone())))),
                            )
                            .child(widgets::caption("Yours", cx).text_xs()),
                    )
                    .child(
                        Button::new(SharedString::from(format!("gp-change:{pi}")))
                            .xsmall()
                            .outline()
                            .label("Change")
                            .tooltip("Choose who gets this pack")
                            .on_click(cx.listener(|this, _, window, cx| this.open(Page::Packs, window, cx))),
                    )
                    .into_any_element()
            })
            .collect();
        let n_packs = pack_rows.len() + my_rows.len();
        let settings_rows = self.shared_settings_rows(g, cx);
        let gid = g.id.clone();
        // Only the group's maker adds people; say who, where it can't be done.
        let add = if g.me.joined {
            let b = Button::new("group-add").xsmall().ghost().icon(IconName::Plus).label("Add");
            Some(if mine {
                b.disabled(down || !can_add)
                    .tooltip(if down {
                        CANT_CONNECT.to_string()
                    } else if can_add {
                        "Ask another friend into this group".to_string()
                    } else {
                        "All your friends are in this group".to_string()
                    })
                    .on_click(cx.listener(move |this, _, window, cx| this.group_dialog(GroupMode::Add(gid.clone()), None, window, cx)))
            } else {
                b.disabled(true).tooltip(format!("Only {owner_name} can add people"))
            })
        } else {
            None
        };
        let card = |c: Div| c.mx_3().mt_3().p_3().rounded(theme.radius_lg).border_1().border_color(theme.border);
        let setting = g.me.joined.then(|| {
            card(h_flex().gap_3().items_start())
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .gap_0p5()
                        .child(div().text_sm().font_medium().child("Keep my packs the same as the group"))
                        .child(widgets::fine_print(
                            "This PC gets the group's packs, and when anyone changes a mod in one, your copy changes too. Your other instances don't change.",
                            cx,
                        )),
                )
                .child(crate::switch::Switch::new("group-step").checked(in_step).on_click({
                    let store = self.store.clone();
                    move |v: &bool, _, cx| {
                        let (id, v) = (step_id.clone(), *v);
                        store.update(cx, |s, cx| s.set_in_step(&id, v, cx))
                    }
                }))
        });
        let ask = (!g.me.joined).then(|| self.invitation(0, g, down, cx));
        let zone = self.take_out_zone(g, cx);
        let top = match zone {
            Some(z) => v_flex().flex_none().child(top).child(z).into_any_element(),
            None => top,
        };
        let body = v_flex()
            .pb_2()
            .children(ask)
            .children(setting)
            .child(cap("Packs", (n_packs > 0).then_some(n_packs), None, cx))
            .children(pack_rows)
            .children(my_rows)
            .when(n_packs == 0, |c| {
                c.child(div().px_3().child(widgets::fine_print(
                    if g.me.joined { "Nobody has shared a pack with the group yet. Share yours under Packs on the left." } else { "Join to get the packs the group shares." },
                    cx,
                )))
            })
            .when(!settings_rows.is_empty(), |c| c.child(cap("Shared settings", Some(settings_rows.len()), None, cx)).children(settings_rows))
            .child(cap("People", Some(member_rows.len()), add.map(|b| b.into_any_element()), cx))
            .children(member_rows)
            .into_any_element();
        (Some(top), body)
    }

    /// The mods whose settings the group shares, in this PC's copies of
    /// its packs: who started sharing each, and Stop.
    fn shared_settings_rows(&self, g: &Group, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let s = self.store.read(cx);
        if !s.settings_sharing() {
            return Vec::new();
        }
        let me = s.mailbox_id();
        let mut out = Vec::new();
        for c in s.settings().group_copies.iter().filter(|c| c.group == g.id) {
            let Some(inst) = s.instances().iter().find(|i| i.id == c.local) else { continue };
            for (m, mc) in c.configs.shared() {
                let a = inst.addons.iter().find(|a| hopper::groupsync::addon_key(a) == *m);
                let (name, icon, pid) = match a {
                    Some(a) => (a.name.clone(), s.image(&a.icon).map(Path::to_path_buf), a.project_id),
                    None => ("A mod you don't have".to_string(), None, 0),
                };
                let n = c.configs.count(m);
                let who = if mc.shared.by == me { "you".to_string() } else { name_or(&mc.shared.name, "a friend") };
                let what = match n {
                    0 => format!("Shared by {who} · no settings changed yet"),
                    1 => format!("Shared by {who} · 1 setting changed"),
                    n => format!("Shared by {who} · {n} settings changed"),
                };
                let kept = s.settings_kept(&inst.root, m);
                let ix = out.len();
                let (root, key, store, view) = (inst.root.clone(), m.clone(), self.store.clone(), cx.entity());
                out.push(
                    h_flex()
                        .id(SharedString::from(format!("gs:{ix}")))
                        .gap_2()
                        .px_3()
                        .py_1()
                        .hover(|s| widgets::row_hover(s, cx))
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .child(div().text_sm().child(widgets::mod_link(("gs-name", ix), name, icon.as_deref(), pid, move |p, _, cx| view.update(cx, |_, cx| cx.emit(FriendsEvent::Project(p))), cx)))
                                .child(widgets::caption(format!("{what} · {}", inst.name), cx).text_xs().truncate()),
                        )
                        .when(kept, |r| r.child(widgets::chip(cx.theme().muted_foreground).child("Keeping yours")))
                        .child(
                            Button::new(SharedString::from(format!("gs-stop:{ix}")))
                                .xsmall()
                                .outline()
                                .label("Stop")
                                .tooltip("Stop sharing this mod's settings. Everyone keeps theirs as they are now.")
                                .on_click(move |_, _, cx| store.update(cx, |s, cx| s.set_settings_shared(&root, &key, false, cx))),
                        )
                        .into_any_element(),
                );
            }
        }
        out
    }

    /// The live invite, while the + popover is shut: one line with the code
    /// to copy, until a friend uses it or it runs out.
    fn invite_reminder(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.plus_pop || self.invite_hidden {
            return None;
        }
        let inv = self.plain_invite(cx)?;
        self.tick(cx);
        let theme = cx.theme().clone();
        let left = inv.until.saturating_duration_since(Instant::now()).as_secs();
        let shown = if inv.code.len() == 8 { format!("{} {}", &inv.code[..4], &inv.code[4..]) } else { inv.code.clone() };
        let copy = inv.code.clone();
        Some(
            div()
                .px_3()
                .pt_2()
                .child(
                    h_flex()
                        .gap_2()
                        .px_2p5()
                        .py_1p5()
                        .rounded(theme.radius_lg)
                        .border_1()
                        .border_color(theme.border)
                        .child(Icon::new(IconName::UserPlus).small().flex_none().text_color(theme.primary))
                        .child(
                            h_flex()
                                .flex_1()
                                .min_w_0()
                                .gap_1()
                                .text_xs()
                                .whitespace_nowrap()
                                .overflow_hidden()
                                .child("Invite code")
                                .child(div().font_semibold().font_family(theme.mono_font_family.clone()).child(shown))
                                .child(format!("· {:02}:{:02} left", left / 60, left % 60)),
                        )
                        .child(
                            Button::new("reminder-copy")
                                .xsmall()
                                .outline()
                                .icon(IconName::Copy)
                                .label("Copy")
                                .tooltip("Copy the code to send it in a chat or message")
                                .on_click(move |_, window, cx| {
                                    cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()));
                                    window.push_notification("Code copied. Send it to your friend.", cx);
                                }),
                        )
                        .child(
                            Button::new("reminder-hide")
                                .xsmall()
                                .ghost()
                                .icon(IconName::Close)
                                .tooltip("Hide this. The code works until it runs out.")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.invite_hidden = true;
                                    cx.notify();
                                })),
                        ),
                )
                .into_any_element(),
        )
    }

    /// No friends, nothing shared and no group yet: what Friends does, the
    /// Invite button (it opens the invite popover under itself) and a way
    /// to paste a friend's code.
    fn empty(&mut self, down: bool, cx: &mut Context<Self>) -> AnyElement {
        let anchor = self.plus_anchor.clone();
        let invite = div()
            .relative()
            .w_full()
            .child(
                Button::new("empty-invite")
                    .w_full()
                    .primary()
                    .icon(IconName::UserPlus)
                    .label("Invite a friend")
                    .selected(self.plus_pop)
                    .disabled(down)
                    .tooltip(if down { CANT_CONNECT } else { INVITE_TIP })
                    // The code comes up in the popover under the button, so
                    // the popover has no second Invite button.
                    .on_click(cx.listener(|this, _, _, cx| {
                        if this.plus_pop && !this.join_only {
                            return this.set_plus(false, cx);
                        }
                        this.invite_hidden = false;
                        if this.plain_invite(cx).is_none() {
                            this.store.update(cx, |s, cx| s.make_invite("", cx));
                        }
                        this.join_only = false;
                        this.set_plus(true, cx)
                    })),
            )
            .child(canvas(move |b, _, _| *anchor.borrow_mut() = Some(b), |_, _, _, _| {}).absolute().top_0().left_0().size_full());
        let join_anchor = self.join_anchor.clone();
        let paste = div()
            .id("empty-paste")
            .relative()
            .text_sm()
            .text_color(crate::theme::ink(cx.theme().primary))
            .clickable(cx)
            .hover(|s| s.underline())
            .child("Got a code from a friend? Paste it here")
            .child(canvas(move |b, _, _| *join_anchor.borrow_mut() = Some(b), |_, _, _, _| {}).absolute().top_0().left_0().size_full())
            .on_click(cx.listener(|this, _, window, cx| {
                this.set_plus(true, cx);
                this.join_only = true;
                this.join_code.update(cx, |s, cx| s.focus(window, cx));
            }));
        let reminder = self.invite_reminder(cx);
        v_flex()
            .w_full()
            .children(reminder)
            .child(v_flex()
            .w_full()
            .px_4()
            .pt_6()
            .gap_4()
            .child(
                v_flex()
                    .gap_1()
                    .child(div().text_base().font_semibold().child("Play the same packs as your friends"))
                    .child(widgets::fine_print("Your friends and your groups show up here.", cx)),
            )
            .child(how_it_works(cx))
            .child(v_flex().gap_2().items_center().child(invite).child(paste))
            .child(widgets::fine_print(
                "Nothing to sign up for. Hopper only sends your friend codes and mod lists. Your worlds and other files stay on your PC.",
                cx,
            )))
            .into_any_element()
    }

    /// Your packs: each with the faces of who gets it and its Share button,
    /// under the "every pack starts private" line until it's closed.
    fn my_packs(&mut self, down: bool, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let s = self.store.read(cx);
        let me = s.mailbox_id();
        let friends = s.mailbox_friends().to_vec();
        let groups: Vec<Group> = s.mailbox_groups().iter().filter(|g| g.me.joined).cloned().collect();
        let insts: Vec<Instance> = s.instances().iter().filter(|i| !i.id.is_empty()).cloned().collect();
        let explain = !s.settings().share_prompt_done;
        if insts.is_empty() {
            return v_flex()
                .child(cap("Your packs", None, None, cx))
                .child(div().px_3().pb_1().child(widgets::fine_print("Install a pack, and it shows up here to share with your friends.", cx)))
                .into_any_element();
        }
        let rows: Vec<AnyElement> = insts
            .iter()
            .enumerate()
            .map(|(i, inst)| {
                let s = self.store.read(cx);
                // Friends first, then groups.
                let direct: Vec<&Friend> = friends.iter().filter(|f| s.settings().shares_with(&inst.id, &f.id)).collect();
                let in_groups: Vec<&Group> = groups.iter().filter(|g| s.settings().shares_with_group(&inst.id, &g.id)).collect();
                let who: Vec<String> = direct.iter().map(|f| name_or(&f.name, "a friend")).chain(in_groups.iter().map(|g| g.name.clone())).collect();
                let sub = match who.len() {
                    0 => "Only you".to_string(),
                    1..=3 => who.join(", "),
                    n => format!("{}, {} and {} more", who[0], who[1], n - 2),
                };
                // Everyone who gets it, each once.
                let mut people: Vec<(String, String, String)> = direct.iter().map(|f| (f.id.clone(), f.mc_id.clone(), name_or(&f.name, "A friend"))).collect();
                for g in &in_groups {
                    for m in g.members.iter().filter(|m| m.id != me && m.joined) {
                        if !people.iter().any(|p| p.0 == m.id) {
                            people.push((m.id.clone(), m.mc_id.clone(), name_or(&m.name, "A friend")));
                        }
                    }
                }
                let people: Vec<(String, String)> = people.into_iter().map(|(_, mc, n)| (mc, n)).collect();
                let faces = (!people.is_empty()).then(|| faces_of(&self.store, &people, theme.background, rems(1.25), cx));
                let s = self.store.read(cx);
                let icon = inst.icon.as_deref().and_then(|u| s.image(u)).map(Path::to_path_buf);
                let open = self.share_pop.as_deref() == Some(inst.id.as_str());
                let kept = inst.only_here.len();
                let (root, inst_id) = (inst.root.clone(), inst.id.clone());
                let anchors = self.anchors.clone();
                let key = inst.id.clone();
                let view = cx.entity();
                h_flex()
                    .id(("my-pack", i))
                    .gap_2()
                    .px_3()
                    .py_1p5()
                    .hover(|s| widgets::row_hover(s, cx))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(
                                widgets::mod_name(("my-pack-name", i), inst.name.clone(), icon.as_deref(), cx)
                                    .text_sm()
                                    .clickable(cx)
                                    .hover(|s| s.text_color(crate::theme::ink(theme.primary)))
                                    .on_click(move |_, _, cx| {
                                        let root = root.clone();
                                        view.update(cx, |_, cx| cx.emit(FriendsEvent::Show(root)))
                                    }),
                            )
                            .child(
                                h_flex()
                                    .min_w_0()
                                    .gap_1()
                                    .pl(rems(1.625))
                                    .child(
                                        widgets::caption(sub, cx)
                                            .text_xs()
                                            .truncate()
                                            .when(!who.is_empty(), |d| d.text_color(theme.foreground)),
                                    )
                                    .when(kept > 0, |r| {
                                        let id = inst.id.clone();
                                        r.child(widgets::caption("·", cx).text_xs().flex_none()).child(
                                            div()
                                                .id(("my-pack-kept", i))
                                                .flex_none()
                                                .text_xs()
                                                .text_color(crate::theme::ink(theme.info))
                                                .clickable(cx)
                                                .hover(|s| s.underline())
                                                .child(format!("{kept} mod{} only on this PC", if kept == 1 { "" } else { "s" }))
                                                .on_click(cx.listener(move |this, _, _, cx| this.set_pop(Some(id.clone()), cx))),
                                        )
                                    }),
                            ),
                    )
                    .children(faces)
                    .child(
                        div()
                            .relative()
                            .flex_none()
                            .child(
                                Button::new(("share", i))
                                    .xsmall()
                                    .outline()
                                    .icon(IconName::Share2)
                                    .label("Share")
                                    .selected(open)
                                    .disabled(down)
                                    .tooltip(if down { CANT_CONNECT } else { "Choose which friends get this pack" })
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        let open = this.share_pop.as_deref() == Some(inst_id.as_str());
                                        this.set_pop((!open).then(|| inst_id.clone()), cx);
                                    })),
                            )
                            .child(
                                canvas(
                                    move |b, _, _| {
                                        anchors.borrow_mut().insert(key.clone(), b);
                                    },
                                    |_, _, _, _| {},
                                )
                                // Pinned to the button: without top/left an absolute
                                // box sits below it, and the popover lands a button low.
                                .absolute()
                                .top_0()
                                .left_0()
                                .size_full(),
                            ),
                    )
                    .into_any_element()
            })
            .collect();
        v_flex()
            .child(cap("Your packs", Some(rows.len()), None, cx))
            .when(explain, |c| {
                c.child(
                    h_flex()
                        .mx_3()
                        .mb_1()
                        .gap_2()
                        .items_start()
                        .px_2p5()
                        .py_1p5()
                        .rounded(theme.radius_lg)
                        .bg(theme.muted)
                        .child(div().flex_1().min_w_0().child(widgets::fine_print(
                            "Every pack starts private. Share one, and those friends get it plus every change you make. Only the mod list is sent. The mods download from their own sites.",
                            cx,
                        )))
                        .child(
                            Button::new("explain-hide")
                                .xsmall()
                                .ghost()
                                .icon(IconName::Close)
                                .tooltip("Got it")
                                .on_click(cx.listener(|this, _, _, cx| this.store.update(cx, |s, cx| s.dismiss_share_prompt(cx)))),
                        ),
                )
            })
            .children(rows)
            .into_any_element()
    }
}

// ---- the rail and its pages ----------------------------------------------------

/// A small mark on a rail tile: news (a dot), or a pack that needs a look.
#[derive(Clone, Copy)]
enum TileMark {
    Dot(Hsla),
    Alert(Hsla),
}

impl FriendsPanel {
    /// Nobody yet: no friends, nothing shared with this PC and no group.
    fn nobody(&self, cx: &App) -> bool {
        let s = self.store.read(cx);
        s.mailbox_friends().is_empty() && s.mailbox_recipes().is_empty() && s.mailbox_groups().is_empty()
    }

    /// One tile on the rail: its art in a rounded square, centred in a slot
    /// as wide as the rail. The open page's slot gets the selected look
    /// (tint and the 3px bar on the rail's edge), as Discord marks the open
    /// server. Clicking opens `page`.
    #[allow(clippy::too_many_arguments)]
    fn tile(&self, id: SharedString, selected: bool, tip: SharedString, art: AnyElement, mark: Option<TileMark>, page: Option<Page>, cx: &mut Context<Self>) -> Stateful<Div> {
        let theme = cx.theme().clone();
        let face = div()
            .relative()
            .flex_none()
            .size(rems(TILE))
            .rounded(theme.radius_lg)
            // A see-through fill: it shows on light and dark panels alike.
            .bg(theme.foreground.opacity(TILE_FILL))
            .flex()
            .items_center()
            .justify_center()
            .text_color(if selected { theme.foreground } else { theme.muted_foreground })
            .child(art)
            .when_some(mark, |d, m| match m {
                TileMark::Dot(c) => d.child(
                    div().absolute().top(px(-2.)).right(px(-2.)).size(px(10.)).rounded_full().border_2().border_color(theme.background).bg(c),
                ),
                TileMark::Alert(c) => d.child(
                    div()
                        .absolute()
                        .top(px(-4.))
                        .right(px(-4.))
                        .size(px(16.))
                        .rounded_full()
                        .border_2()
                        .border_color(theme.background)
                        .bg(c)
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_color(theme.background)
                        .text_size(px(10.))
                        .font_bold()
                        .child("!"),
                ),
            });
        let tip2 = tip.clone();
        h_flex()
            .id(id)
            .w_full()
            .flex_none()
            .justify_center()
            .py(px(3.))
            .when(selected, |d| widgets::row_selected(d, cx))
            .when(!selected, |d| d.hover(|s| widgets::row_hover(s, cx)))
            .tooltip(move |w, cx| gpui_kit::component::tooltip::Tooltip::new(tip2.clone()).build(w, cx))
            .when_some(page, |d, page| {
                d.clickable(cx).on_click(cx.listener(move |this, _, window, cx| this.open(page.clone(), window, cx)))
            })
            .child(face)
    }

    /// A group's tile art: two members' faces, or the group's initials
    /// when nobody else is in it yet.
    fn group_art(&self, g: &Group, ring: Hsla, cx: &mut Context<Self>) -> AnyElement {
        let me = self.store.read(cx).mailbox_id();
        // Faces only when there are real ones: two blank heads say less
        // than the group's initials.
        let others: Vec<(String, String)> =
            g.members.iter().filter(|m| m.id != me && m.joined && !m.mc_id.is_empty()).take(2).map(|m| (m.mc_id.clone(), m.name.clone())).collect();
        if others.is_empty() {
            return div().text_sm().font_semibold().child(initials(&g.name)).into_any_element();
        }
        let faces: Vec<AnyElement> = others.iter().map(|(mc, n)| face(&self.store, mc, n, rems(1.125), cx)).collect();
        widgets::face_stack(faces, ring, rems(1.125), cx.theme().radius).into_any_element()
    }

    /// The rail: everyone, each group (invitations first), a new group,
    /// and the packs at the foot.
    fn rail(&self, groups: &[Group], cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let everyone = matches!(self.page, Page::Everyone | Page::Friend(_));
        let mut order: Vec<&Group> = groups.iter().filter(|g| !g.me.joined).collect();
        order.extend(groups.iter().filter(|g| g.me.joined));
        let sync = sync_rows(self.store.read(cx));
        let mut tiles: Vec<AnyElement> = Vec::new();
        for g in order {
            let open = self.page == Page::Group(g.id.clone());
            let art = self.group_art(g, theme.background, cx);
            let alert = alert_of(sync.iter().filter(|r| r.group == g.id), cx);
            let (tip, mark) = if g.me.joined {
                match alert {
                    Some(_) => (format!("{}: a pack needs a look", g.name), alert),
                    None => (g.name.clone(), None),
                }
            } else {
                (format!("{}: you're asked to join", g.name), Some(TileMark::Dot(theme.primary)))
            };
            let t = self.tile(format!("rail-g:{}", g.id).into(), open, tip.into(), art, mark, Some(Page::Group(g.id.clone())), cx);
            let texts = self.dragging.as_ref().map(|d| self.drop_text(g, d, cx));
            let gid = g.id.clone();
            let t = self.drop_target(t, texts, move |this, d, window, cx| this.drop_on(&gid, d, window.modifiers().control, window, cx), cx);
            tiles.push(t.into_any_element());
        }
        let can_group = self.can_group(cx);
        let plus = self
            .tile(
                "rail-new".into(),
                false,
                if can_group { "New group".into() } else { "Make a group once you have a friend".into() },
                Icon::new(IconName::Plus).small().into_any_element(),
                None,
                None,
                cx,
            )
            .when(!can_group, |d| d.opacity(0.5))
            .when(can_group, |d| {
                d.clickable(cx).on_click(cx.listener(|this, _, window, cx| this.group_dialog(GroupMode::New, None, window, cx)))
            });
        // Dropped on +: a new group with them in it.
        let texts = self.dragging.as_ref().map(|d| {
            let t = if can_group { format!("New group with {}", d.name) } else { CANT_CONNECT.to_string() };
            (t.clone(), t, can_group)
        });
        let plus = self.drop_target(
            plus,
            texts,
            |this, d, window, cx| {
                if this.can_group(cx) {
                    this.group_dialog(GroupMode::New, Some(d.id.clone()), window, cx)
                }
            },
            cx,
        );
        let packs_open = self.page == Page::Packs;
        let packs_alert = alert_of(sync.iter(), cx);
        v_flex()
            .flex_none()
            .w(rems(RAIL_W))
            .h_full()
            .border_r_1()
            .border_color(theme.border)
            .py_1p5()
            .gap_1()
            .child(self.tile(
                "rail-everyone".into(),
                everyone,
                "Everyone".into(),
                Icon::new(IconName::Users).small().into_any_element(),
                None,
                Some(Page::Everyone),
                cx,
            ))
            .child(div().flex_none().mx_auto().w(rems(1.5)).h_px().my_0p5().bg(theme.border))
            .child(v_flex().id("rail-groups").flex_1().min_h_0().overflow_y_scroll().gap_1().children(tiles).child(plus))
            .child(self.tile(
                "rail-packs".into(),
                packs_open,
                if packs_alert.is_some() { "Packs: one needs a look".into() } else { "Packs: yours and your friends'".into() },
                Icon::new(IconName::Package).small().into_any_element(),
                packs_alert,
                Some(Page::Packs),
                cx,
            ))
            .into_any_element()
    }

    /// The bar above a page: its title, and something on the right.
    fn page_head(&self, title: impl Into<SharedString>, right: Option<AnyElement>, cx: &App) -> AnyElement {
        h_flex()
            .flex_none()
            .h(rems(2.5))
            .pl_3()
            .pr_2()
            .gap_2()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(div().flex_1().min_w_0().truncate().text_sm().font_semibold().child(title.into()))
            .children(right)
            .into_any_element()
    }

    /// What's beside the rail: the open page's top and its scrolling body.
    fn page_body(&mut self, down: bool, window: &mut Window, cx: &mut Context<Self>) -> (Option<AnyElement>, AnyElement) {
        let s = self.store.read(cx);
        let friends = s.mailbox_friends().to_vec();
        let all_recipes = s.mailbox_recipes().to_vec();
        let recipes: Vec<Event> = all_recipes.iter().filter(|e| !s.pack_hidden(e)).cloned().collect();
        let groups: Vec<Group> = s.mailbox_groups().to_vec();
        if self.page == Page::Packs {
            let top = self.page_head("Packs", None, cx);
            return (Some(top), self.packs_view(&friends, &recipes, &all_recipes, down, window, cx));
        }
        if self.nobody(cx) {
            self.page = Page::Everyone;
            return (None, self.empty(down, cx));
        }
        let people = self.people(&recipes, &groups, down, cx);
        match self.page.clone() {
            Page::Friend(id) => {
                if let Some(p) = people.iter().find(|p| p.id == id) {
                    return self.friend_page(p, down, window, cx);
                }
            }
            Page::Group(id) => {
                if let Some(g) = groups.iter().find(|g| g.id == id) {
                    return self.group_page(g, &all_recipes, down, window, cx);
                }
            }
            Page::Everyone | Page::Packs => {}
        }
        // The page's friend or group is gone: everyone again.
        self.page = Page::Everyone;
        let online = people.iter().filter(|p| p.friend.as_ref().is_some_and(|f| f.online) && !down).count();
        let right = (online > 0).then(|| widgets::caption(format!("{online} online"), cx).text_xs().flex_none().into_any_element());
        let top = self.page_head("Everyone", right, cx);
        (Some(top), self.everyone(&people, &recipes, down, window, cx))
    }

    /// Everyone: online friends, then offline ones, then people met only
    /// through a group. Searching narrows them and adds the packs that match.
    fn everyone(&mut self, people: &[Person], recipes: &[Event], down: bool, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let s = self.store.read(cx);
        let has_friends = people.iter().any(|p| p.friend.is_some());
        let prompt = has_friends && s.settings().shared_with.is_empty() && !s.settings().share_prompt_done;
        let q = self.search.read(cx).value().to_string();
        let words = widgets::search_words(&q);
        let n_packs: usize = people.iter().map(|p| p.packs.len()).sum();
        let searchable = people.len() > 6 || n_packs > 12 || !words.is_empty();
        let reminder = self.invite_reminder(cx);
        let mut on_rows = Vec::new();
        let mut off_rows = Vec::new();
        let mut other_rows = Vec::new();
        for (i, p) in people.iter().enumerate().filter(|(_, p)| hits(&p.name, &words)) {
            let row = self.friend_row(i, p, &words, down, window, cx);
            match &p.friend {
                Some(f) if f.online && !down => on_rows.push(row),
                Some(_) => off_rows.push(row),
                None => other_rows.push(row),
            }
        }
        let mut pack_hits: Vec<AnyElement> = Vec::new();
        if !words.is_empty() {
            let s = self.store.read(cx);
            let found: Vec<(Event, String)> = people
                .iter()
                .flat_map(|p| p.packs.iter().map(move |e| (e.clone(), p.name.clone())))
                .filter(|(e, _)| hits(&pack_name(e), &words) && s.followers_of(&e.from, &e.inst).is_empty())
                .collect();
            for (i, (e, by)) in found.iter().enumerate() {
                pack_hits.push(self.their_pack(&format!("hit:{i}"), e, Some(by.clone()), false, down, window, cx));
            }
        }
        let _ = recipes;
        let nothing = !words.is_empty() && on_rows.is_empty() && off_rows.is_empty() && other_rows.is_empty() && pack_hits.is_empty();
        let groups_here = self.store.read(cx).mailbox_groups().iter().any(|g| g.me.joined);
        v_flex()
            .pb_2()
            .when(prompt, |c| c.child(div().px_3().pt_3().child(self.prompt(cx))))
            .children(reminder)
            .when(searchable, |c| c.child(search_box(&self.search)))
            .when(!on_rows.is_empty(), |c| c.child(cap("Online", Some(on_rows.len()), None, cx)).children(on_rows))
            .when(!off_rows.is_empty(), |c| c.child(cap("Offline", Some(off_rows.len()), None, cx)).children(off_rows))
            .when(!other_rows.is_empty(), |c| c.child(cap("In your groups", Some(other_rows.len()), None, cx)).children(other_rows))
            .when(!pack_hits.is_empty(), |c| c.child(cap("Packs", Some(pack_hits.len()), None, cx)).children(pack_hits))
            .when(nothing, |c| c.child(div().px_3().py_3().child(widgets::fine_print(format!("Nothing matches \u{201c}{q}\u{201d}."), cx))))
            .when(words.is_empty() && has_friends, |c| {
                c.child(div().px_3().py_3().child(widgets::fine_print(
                    if groups_here {
                        "Open a friend to see the packs they share with you. Drag a friend onto a group on the left to add them."
                    } else {
                        "Open a friend to see the packs they share with you."
                    },
                    cx,
                )))
            })
            .into_any_element()
    }

    /// The Packs page: this PC's packs and who gets each, the friends'
    /// packs (followed ones first), and the follows that stopped.
    /// The top of the Packs page when a followed copy needs the player:
    /// which pack, why, and the way on. Copies that are fine or only wait
    /// for the game aren't listed; the header's chip covers those.
    fn sync_card(&self, recipes: &[Event], down: bool, cx: &mut Context<Self>) -> Option<AnyElement> {
        let s = self.store.read(cx);
        let need: Vec<SyncRow> = sync_rows(s).into_iter().filter(|r| r.state.rank() <= 1).collect();
        if need.is_empty() {
            return None;
        }
        let theme = cx.theme().clone();
        let failed = matches!(need[0].state, CopyState::Failed(..));
        let tint = if failed { theme.danger } else { theme.warning };
        let title = match need.len() {
            1 => "A pack you follow needs a look".to_string(),
            n => format!("{n} packs you follow need a look"),
        };
        let mut rows: Vec<AnyElement> = Vec::new();
        for (i, r) in need.iter().enumerate() {
            let s = self.store.read(cx);
            let note = r.state.note(&r.who).unwrap_or_default();
            // Mods to ask about: Update asks. Anything else: the pack's page.
            let held_event = matches!(r.state, CopyState::Held(_))
                .then(|| recipes.iter().find(|e| s.followers_of(&e.from, &e.inst).iter().any(|f| Path::new(&f.root) == r.root)).cloned())
                .flatten();
            let action: AnyElement = match held_event {
                Some(e) => self.update_button(format!("sync-update:{i}").into(), &e, r.root.clone(), down, cx).into_any_element(),
                None => {
                    let root = r.root.clone();
                    Button::new(("sync-open", i))
                        .xsmall()
                        .outline()
                        .label("Open")
                        .tooltip("Open the pack's page")
                        .on_click(cx.listener(move |_, _, _, cx| cx.emit(FriendsEvent::Show(root.clone()))))
                        .into_any_element()
                }
            };
            let icon = if matches!(r.state, CopyState::Failed(..)) { (IconName::TriangleAlert, theme.danger) } else { (IconName::Info, theme.warning) };
            rows.push(
                h_flex()
                    .gap_2()
                    .items_start()
                    .child(Icon::new(icon.0).xsmall().flex_none().mt(px(3.)).text_color(chip_ink(icon.1, cx)))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(div().text_sm().font_medium().truncate().child(r.name.clone()))
                            .child(div().text_xs().text_color(theme.muted_foreground).child(note)),
                    )
                    .child(action)
                    .into_any_element(),
            );
        }
        Some(
            v_flex()
                .mx_3()
                .mt_2()
                .mb_1()
                .p_2()
                .gap_2()
                .rounded(theme.radius)
                .bg(tint.opacity(widgets::alpha::TINT))
                .child(div().text_sm().font_semibold().child(title))
                .children(rows)
                .into_any_element(),
        )
    }

    fn packs_view(&mut self, friends: &[Friend], recipes: &[Event], all_recipes: &[Event], down: bool, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let mine = self.my_packs(down, cx);
        let me = self.store.read(cx).mailbox_id();
        let following = self.following(recipes, &[], down, cx);
        // The rest of what friends share, each pack once however it came.
        let s = self.store.read(cx);
        let mut rest: Vec<Event> = recipes.iter().filter(|e| e.from != me && s.followers_of(&e.from, &e.inst).is_empty()).cloned().collect();
        rest.sort_by_key(|e| std::cmp::Reverse(e.at));
        let rest_rows: Vec<AnyElement> = rest
            .iter()
            .enumerate()
            .map(|(i, e)| {
                let by = self.via(e, cx);
                self.their_pack(&format!("pk:{i}"), e, Some(by), false, down, window, cx)
            })
            .collect();
        let ended = self.ended_list(friends, all_recipes, cx);
        let ended_rows = self.ended_rows(ended, cx);
        let nothing_in = following.is_empty() && rest_rows.is_empty();
        let card = self.sync_card(recipes, down, cx);
        v_flex()
            .pb_2()
            .children(card)
            .child(mine)
            .child(cap("From friends", (!nothing_in).then_some(following.len() + rest_rows.len()), None, cx))
            .when(nothing_in, |c| c.child(div().px_3().pb_1().child(widgets::fine_print("Packs your friends share with you show up here.", cx))))
            .children(following)
            .children(rest_rows)
            .when(!ended_rows.is_empty(), |c| {
                c.child(cap("Won't change any more", Some(ended_rows.len()), None, cx))
                    .child(div().px_3().pb_1().child(widgets::fine_print(
                        "The friend removed you, or stopped sharing the pack. Your copies stay on your PC as they are.",
                        cx,
                    )))
                    .children(ended_rows)
            })
            .into_any_element()
    }

    /// How a friend's pack reached this PC: the group's name, or "Sam".
    fn via(&self, e: &Event, cx: &App) -> String {
        let s = self.store.read(cx);
        let sender = name_or(&e.from_name, "a friend");
        let g = e.groups.iter().find_map(|g| s.mailbox_groups().iter().find(|x| &x.id == g && x.me.joined).map(|x| x.name.clone()));
        match g {
            Some(g) if !e.direct => g,
            Some(g) => format!("{sender} and {g}"),
            None => sender,
        }
    }

    /// The stopped follows, each with Stop following.
    fn ended_rows(&self, ended: Vec<(PathBuf, String, String, String)>, cx: &mut Context<Self>) -> Vec<AnyElement> {
        ended
            .into_iter()
            .enumerate()
            .map(|(i, (root, pack, local, who))| {
                h_flex()
                    .id(("ended", i))
                    .gap_2()
                    .px_3()
                    .py_1()
                    .hover(|s| widgets::row_hover(s, cx))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(div().text_sm().truncate().child(pack))
                            .child(widgets::caption(format!("{who} · your copy: {local}"), cx).text_xs()),
                    )
                    .child(
                        Button::new(("stop-follow", i))
                            .xsmall()
                            .outline()
                            .label("Stop following")
                            .tooltip("Take it off this list. Your copy stays on your PC.")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                let root = root.clone();
                                this.store.update(cx, |s, cx| s.stop_following(&root, cx))
                            })),
                    )
                    .into_any_element()
            })
            .collect()
    }
}

/// A group's initials for its tile, as Discord does for a server without
/// a picture: the first letters of its first two words ("drunk gamblers"
/// is DG), or the first letter of a one-word name.
fn initials(name: &str) -> String {
    let firsts: Vec<char> = name.split_whitespace().filter_map(|w| w.chars().find(|c| c.is_alphanumeric())).take(2).collect();
    match firsts.len() {
        0 => "?".to_string(),
        _ => firsts.into_iter().flat_map(char::to_uppercase).collect(),
    }
}

/// Up to three faces of who gets something, overlapped, and how many more.
/// (gpui-kit's `AvatarGroup` clips the initials at any size but its fixed
/// pixel ones, which don't follow the zoom.)
fn faces_of(store: &Entity<Store>, people: &[(String, String)], ring: Hsla, size: Rems, cx: &mut App) -> Div {
    let faces: Vec<AnyElement> = people.iter().take(3).map(|(mc, n)| face(store, mc, n, size, cx)).collect();
    let more = people.len().saturating_sub(3);
    let corner = cx.theme().radius;
    h_flex()
        .flex_none()
        .gap_1()
        .child(widgets::face_stack(faces, ring, size, corner))
        .when(more > 0, |d| d.child(widgets::caption(format!("+{more}"), cx).text_xs()))
}

impl Render for FriendsPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        self.spots.borrow_mut().clear();
        if self.dragging.is_some() && !cx.has_active_drag() {
            self.dragging = None;
        }
        let s = self.store.read(cx);
        let status = s.sync_status();
        let heard = !s.mailbox_friends().is_empty() || !s.mailbox_recipes().is_empty() || !s.mailbox_groups().is_empty();
        let groups: Vec<Group> = s.mailbox_groups().to_vec();
        let down = matches!(status, SyncStatus::Down(_));
        let body: AnyElement = match &status {
            SyncStatus::Off => self.off(cx),
            SyncStatus::Connecting if !heard => {
                v_flex().p_3().child(widgets::skeleton_rows(5, rems(2.75), cx)).into_any_element()
            }
            _ => {
                // Down before the relay ever answered: the friend list isn't
                // known, so don't show the "no friends yet" pitch.
                let (top, content) = if down && !heard && self.page != Page::Packs {
                    (None, div().into_any_element())
                } else {
                    self.page_body(down, window, cx)
                };
                // Each page keeps its own scroll position, and a fresh one
                // starts at the top.
                let scroll_id = match &self.page {
                    Page::Friend(id) => SharedString::from(format!("friends-scroll:f:{id}")),
                    Page::Group(id) => SharedString::from(format!("friends-scroll:g:{id}")),
                    Page::Packs => SharedString::from("friends-scroll:packs"),
                    Page::Everyone => SharedString::from("friends-scroll"),
                };
                let main = v_flex()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .when_some(match &status {
                        SyncStatus::Down(e) => Some(e.clone()),
                        _ => None,
                    }, |c, e| {
                        c.child(
                            div().p_3().flex_none().child(
                                widgets::notice_bar(theme.warning, IconName::CloudOff, cx).px_3().items_start().child(
                                    v_flex()
                                        .flex_1()
                                        .min_w_0()
                                        .child(div().text_sm().font_medium().child("Can't connect right now"))
                                        .child(widgets::fine_print(
                                            // The plain "can't reach" message would repeat the title.
                                            match (heard, e == CANT_CONNECT) {
                                                (false, _) => "We'll keep trying. Your friends show up here once Hopper connects.".to_string(),
                                                (true, true) => "Showing what you had last time. We'll keep trying.".to_string(),
                                                (true, false) => format!("Showing what you had last time. {e}"),
                                            },
                                            cx,
                                        )),
                                ),
                            ),
                        )
                    })
                    .children(top)
                    .child(div().id(scroll_id).flex_1().min_h_0().overflow_y_scrollbar().child(content));
                h_flex().size_full().items_start().child(self.rail(&groups, cx)).child(main).into_any_element()
            }
        };
        let content = v_flex()
            .size_full()
            .track_focus(&self.focus)
            .on_modifiers_changed(cx.listener(|this, _, window, _| {
                if this.dragging.is_some() {
                    window.refresh();
                }
            }))
            // Escape on a page goes back to everyone.
            .on_key_down(cx.listener(|this, e: &KeyDownEvent, window, cx| {
                if e.keystroke.key != "escape" {
                    return;
                }
                // A popover closes first, then the page goes back.
                if this.plus_pop || this.share_pop.is_some() {
                    this.set_plus(false, cx);
                    this.set_pop(None, cx);
                    cx.stop_propagation();
                } else if this.page != Page::Everyone {
                    this.open(Page::Everyone, window, cx);
                    cx.stop_propagation();
                }
            }))
            .child(self.header(cx))
            .child(div().flex_1().min_h_0().child(body));
        // The same dock as the mod peek: its width, edge, border and look.
        let _ = &theme;
        widgets::dock(SharedString::from("friends-dock"), &self.panel, |t: &mut Self| &mut t.panel, &self.store, content, window, cx)
    }
}

/// A chip's text colour. On a light theme orange and green text on their
/// own faint tint is too pale to read (2.8:1 for the orange), so it is
/// darkened there until the 12px text reads at 4.5:1 on the tint.
fn chip_ink(c: Hsla, cx: &App) -> Hsla {
    if cx.theme().mode.is_dark() { c } else { hsla(c.h, c.s, (c.l - 0.13).max(0.), c.a) }
}

/// "or" between two ways in, with a line either side.
fn or_line(cx: &App) -> Div {
    let line = cx.theme().border;
    h_flex()
        .gap_2()
        .child(div().flex_1().h_px().bg(line))
        .child(widgets::caption("or", cx).text_xs())
        .child(div().flex_1().h_px().bg(line))
}

/// Whether a friend is online. With the service out of reach "online" is
/// stale, so it says when they were last seen instead.
fn status(f: Option<&Friend>, down: bool) -> String {
    match f {
        Some(f) if f.online && !down => "Online".to_string(),
        Some(f) if down && f.last_seen == 0 => "Online status unknown".to_string(),
        Some(f) if f.last_seen > 0 => format!("Last online {}", ago(f.last_seen)),
        Some(_) => "Offline".to_string(),
        None => "In a group with you".to_string(),
    }
}

/// A face with the green online dot on its corner. The dot's ring is the
/// dock's colour, so it reads as cut out of the face.
fn with_dot(head: AnyElement, online: bool, cx: &App) -> Div {
    let theme = cx.theme();
    let ring = theme.sidebar;
    let green = theme.success;
    div().relative().flex_none().child(head).when(online, |d| {
        d.child(div().absolute().right(px(-1.)).bottom(px(-1.)).size(px(9.)).rounded_full().border_2().border_color(ring).bg(green))
    })
}

/// A shared pack's name: the one it was shared with, else its instance's.
fn pack_name(e: &Event) -> String {
    if e.name.is_empty() { e.inst.clone() } else { e.name.clone() }
}

/// Whether `text` holds every search word.
fn hits(text: &str, words: &[String]) -> bool {
    let lower = text.to_lowercase();
    words.iter().all(|w| lower.contains(w.as_str()))
}

/// A section's caption: its title and count, and something on the right
/// (a sort menu).
fn cap(title: &'static str, count: Option<usize>, right: Option<AnyElement>, cx: &App) -> Div {
    h_flex()
        .gap_1p5()
        .px_3()
        .pt_2p5()
        .pb_1()
        .child(widgets::caption(title, cx).text_xs().font_medium())
        .children(count.map(|n| widgets::caption(n.to_string(), cx).text_xs()))
        .child(div().flex_1())
        .children(right)
}

/// The panel's search field, under the tabs or a page's top.
fn search_box(state: &Entity<InputState>) -> Div {
    div().px_3().pt_2().child(widgets::search_input(state).small().cleanable(true))
}

/// A friend's face, from their Minecraft account; a plain square in the
/// same shape when they aren't signed in. Its edge line keeps it visible on
/// a light theme, where the fill is almost the panel's own colour.
pub(crate) fn face(store: &Entity<Store>, mc_id: &str, name: &str, size: Rems, cx: &mut App) -> AnyElement {
    if mc_id.is_empty() {
        let theme = cx.theme();
        return div()
            .flex_none()
            .size(size)
            .rounded(theme.radius)
            .border_1()
            .border_color(theme.border)
            .bg(theme.muted)
            .flex()
            .items_center()
            .justify_center()
            .child(Icon::new(IconName::User).xsmall().text_color(theme.muted_foreground))
            .into_any_element();
    }
    let saved = hopper::accounts::Saved { id: mc_id.to_string(), name: name.to_string(), ..Default::default() };
    let url = saved.fallback_avatar();
    store.update(cx, |s, _| s.want_image_now(&url));
    div().flex_none().child(crate::accounts::head(store.read(cx), &saved, size, cx)).into_any_element()
}

// ---- following a friend's pack -------------------------------------------------

/// Where a friend's pack should go.
#[derive(Clone, Debug, PartialEq)]
enum FollowPick {
    Existing(usize),
    New,
}

/// Asks where a friend's pack goes: over an instance it fits, or a new one.
/// Following replaces the mods, so it is never done without asking.
struct FollowChooser {
    store: Entity<Store>,
    req: FollowReq,
    /// Instances the code fits, and whether their pack updates first.
    fitting: Vec<(PathBuf, String, bool)>,
    /// Their pack is a plain instance, so "fits" means the same game and
    /// loader, not the same pack.
    plain: bool,
    pick: FollowPick,
}

/// Opens the follow chooser for `e`.
pub fn open_follow(store: Entity<Store>, e: &Event, window: &mut Window, cx: &mut App) {
    let share = match hopper::sharecode::decode(&e.code) {
        Ok(s) => s,
        Err(err) => {
            window.push_notification(format!("{err:#}"), cx);
            return;
        }
    };
    // Only instances this code fits: the same pack, or a newer version of
    // it. A different pack has to be a new instance.
    let mut fitting = Vec::new();
    for inst in store.read(cx).instances() {
        let update = match hopper::recipe::fit(inst, &share) {
            hopper::recipe::Fit::Different(_) => continue,
            hopper::recipe::Fit::PackUpdate(_) => true,
            hopper::recipe::Fit::Same => false,
        };
        // An instance already following something else stays with it.
        if store.read(cx).follow_at(&inst.root).is_some_and(|f| !f.matches(&e.from, &e.inst, "")) {
            continue;
        }
        fitting.push((inst.root.clone(), inst.name.clone(), update));
    }
    let req = FollowReq::of(e);
    let title = format!("Follow {}'s {}", name_or(&req.peer_name, "a friend"), if req.name.is_empty() { &req.inst } else { &req.name });
    let pick = if fitting.is_empty() { FollowPick::New } else { FollowPick::Existing(0) };
    let plain = matches!(share.base, hopper::sharecode::Base::Plain { .. });
    let view = cx.new(|_| FollowChooser { store: store.clone(), req, fitting, plain, pick });
    window.open_dialog(cx, move |dialog, window, _| {
        let v2 = view.clone();
        dialog
            .title(title.clone())
            .w(rems(34.).to_pixels(window.rem_size()))
            .margin_top(rems(widgets::DIALOG_TOP).to_pixels(window.rem_size()))
            .child(view.clone())
            .footer(crate::dialogs::footer("Follow", true))
            .on_ok(move |_, window, cx| {
                let this = v2.read(cx);
                let target = match this.pick {
                    FollowPick::Existing(ix) => match this.fitting.get(ix) {
                        Some((root, _, _)) => FollowTarget::Existing(root.clone()),
                        None => return true,
                    },
                    FollowPick::New => FollowTarget::New,
                };
                let (store, req) = (this.store.clone(), this.req.clone());
                // The store asks about mods from other sources, then follows.
                crate::store::follow_pack(&store, req, target, window, cx);
                true
            })
    });
}

impl FollowChooser {
    /// One choice: a radio, the instance, what happens, and a chip.
    fn row(
        &self,
        id: SharedString,
        pick: FollowPick,
        icon: Option<PathBuf>,
        name: String,
        what: String,
        chip: Option<(Hsla, &'static str)>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let picked = self.pick == pick;
        let (p1, p2) = (pick.clone(), pick);
        h_flex()
            .id(id.clone())
            .gap_3()
            .px_3()
            .py_2()
            .rounded(theme.radius)
            .clickable(cx)
            .map(|r| if picked { widgets::row_selected(r, cx) } else { r.hover(|s| widgets::row_hover(s, cx)) })
            .on_click(cx.listener(move |this, _, _, cx| {
                this.pick = p1.clone();
                cx.notify();
            }))
            .child(Radio::new(SharedString::from(format!("{id}-radio"))).checked(picked).on_click(cx.listener(
                move |this, _: &bool, _, cx| {
                    this.pick = p2.clone();
                    cx.notify();
                },
            )))
            .child(widgets::thumb(icon.as_deref(), rems(2.), cx))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(div().text_sm().font_medium().truncate().child(name))
                    .child(div().text_xs().text_color(theme.muted_foreground).child(what)),
            )
            .children(chip.map(|(c, label)| widgets::chip(c).child(label)))
            .into_any_element()
    }
}

impl Render for FollowChooser {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let who = name_or(&self.req.peer_name, "Your friend");
        let s = self.store.read(cx);
        let rows: Vec<(PathBuf, String, bool, Option<PathBuf>)> = self
            .fitting
            .iter()
            .map(|(root, name, upd)| {
                let icon = s.instance(root).and_then(|i| i.icon.as_deref()).and_then(|u| s.image(u)).map(Path::to_path_buf);
                (root.clone(), name.clone(), *upd, icon)
            })
            .collect();
        let new_icon = pack_look(s, &self.req.code).0;
        let mut list = v_flex().gap_1();
        for (ix, (_, name, upd, icon)) in rows.iter().enumerate() {
            let (what, chip) = if *upd {
                (format!("{who} has a newer version of this pack. Yours updates to it first."), (theme.primary, "Newer pack"))
            } else {
                let what = if self.plain { "Same Minecraft and mod loader" } else { "Same pack" };
                (what.to_string(), (theme.success, "Matches"))
            };
            list = list.child(self.row(
                format!("follow-at:{ix}").into(),
                FollowPick::Existing(ix),
                icon.clone(),
                name.clone(),
                what,
                Some(chip),
                cx,
            ));
        }
        list = list.child(self.row(
            "follow-new".into(),
            FollowPick::New,
            new_icon,
            "A new instance".into(),
            "Installs a fresh copy of their pack. Your other instances don't change.".into(),
            None,
            cx,
        ));
        let over = match self.pick {
            FollowPick::Existing(ix) => rows.get(ix).map(|r| r.1.clone()),
            FollowPick::New => None,
        };
        v_flex()
            .gap_3()
            .child(div().text_sm().child(format!(
                "Follow: your copy updates when {who} changes theirs. Only the mod list comes from {who}. The mods download from their own sites."
            )))
            .child(widgets::caption("Where should it go?", cx).text_xs().font_medium())
            .child(list)
            .when_some(over, |c, inst| {
                c.child(
                    h_flex()
                        .gap_2()
                        .items_start()
                        .p_3()
                        .rounded(theme.radius)
                        .bg(theme.warning.opacity(widgets::alpha::TINT))
                        .child(Icon::new(IconName::TriangleAlert).small().flex_none().text_color(theme.warning))
                        .child(div().flex_1().min_w_0().text_sm().child(format!(
                            "{inst} gets {who}'s mods and settings now, and again each time {who} changes the pack. Your own changes there are replaced. You can undo each change in History."
                        ))),
                )
            })
            .when(!self.req.left_out.is_empty(), |c| {
                let n = self.req.left_out.len();
                c.child(widgets::fine_print(
                    format!(
                        "{n} file{} can't be downloaded from any site. Ask {who} to send {}.",
                        if n == 1 { "" } else { "s" },
                        if n == 1 { "it" } else { "them" }
                    ),
                    cx,
                ))
            })
    }
}

// ---- friend groups -------------------------------------------------------------

/// What the group dialog does.
#[derive(Clone, PartialEq)]
enum GroupMode {
    New,
    /// Invite more friends into this group (its id).
    Add(String),
    Rename(String),
}

/// "New group": a name and the friends in it. Also adds friends to a
/// group, or renames one.
struct GroupForm {
    store: Entity<Store>,
    mode: GroupMode,
    name: Entity<InputState>,
    /// Friends who can be picked: all of them, or those not in the group.
    friends: Vec<Friend>,
    picked: Vec<String>,
    paint: drag_pick::Paint,
}

/// Opens the group dialog. `pick`: a friend ticked from the start (the
/// friend's ⋯ › New group with them).
fn open_group(store: Entity<Store>, mode: GroupMode, pick: Option<String>, window: &mut Window, cx: &mut App) {
    let s = store.read(cx);
    let group = match &mode {
        GroupMode::Add(id) | GroupMode::Rename(id) => s.mailbox_groups().iter().find(|g| &g.id == id).cloned(),
        GroupMode::New => None,
    };
    let friends: Vec<Friend> = s
        .mailbox_friends()
        .iter()
        .filter(|f| group.as_ref().is_none_or(|g| !g.members.iter().any(|m| m.id == f.id)))
        .cloned()
        .collect();
    let old = group.as_ref().map(|g| g.name.clone()).unwrap_or_default();
    let title = match &mode {
        GroupMode::New => "New group".to_string(),
        GroupMode::Add(_) => format!("Add friends to {old}"),
        GroupMode::Rename(_) => format!("Rename {old}"),
    };
    let form = cx.new(|cx| GroupForm {
        store: store.clone(),
        mode: mode.clone(),
        name: cx.new(|cx| InputState::new(window, cx).placeholder("Weekend crew").default_value(old)),
        friends,
        picked: pick.into_iter().collect(),
        paint: drag_pick::Paint::default(),
    });
    let sub = form.update(cx, |f, cx| cx.observe(&f.name, |_, _, cx| cx.notify()));
    window.open_dialog(cx, move |dialog, window, cx| {
        let _ = &sub;
        let f2 = form.clone();
        let this = form.read(cx);
        let named = !this.name.read(cx).value().trim().is_empty();
        let (action, ready) = match &this.mode {
            GroupMode::New => ("Create group", named && !this.picked.is_empty()),
            GroupMode::Add(_) => ("Add", !this.picked.is_empty()),
            GroupMode::Rename(_) => ("Rename", named),
        };
        dialog
            .title(title.clone())
            .w(rems(34.).to_pixels(window.rem_size()))
            .margin_top(rems(widgets::DIALOG_TOP).to_pixels(window.rem_size()))
            .child(form.clone())
            .footer(crate::dialogs::footer(action, ready))
            .on_ok(move |_, _, cx| {
                let this = f2.read(cx);
                let name = this.name.read(cx).value().trim().to_string();
                let (store, mode, picked) = (this.store.clone(), this.mode.clone(), this.picked.clone());
                store.update(cx, |s, cx| match mode {
                    GroupMode::New => s.create_group(&name, picked, cx),
                    GroupMode::Add(id) => s.add_to_group(&id, picked, cx),
                    GroupMode::Rename(id) => s.rename_group(&id, &name, cx),
                });
                true
            })
    });
}

impl GroupForm {
    fn set(&mut self, id: &str, on: bool) {
        self.picked.retain(|p| p != id);
        if on {
            self.picked.push(id.to_string());
        }
    }

    /// The friends to tick, dragged across like the share popover.
    fn list(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let me = cx.entity();
        let group: SharedString = "group-pick".into();
        if self.friends.is_empty() {
            return widgets::fine_print("All your friends are in this group already.", cx).into_any_element();
        }
        let mut rows = Vec::new();
        for (i, f) in self.friends.clone().into_iter().enumerate() {
            let on = self.picked.contains(&f.id);
            let set = {
                let (me, id) = (me.clone(), f.id.clone());
                Rc::new(move |v: usize, _: &mut Window, cx: &mut App| {
                    me.update(cx, |t, cx| {
                        if t.picked.contains(&id) != (v == 1) {
                            t.set(&id, v == 1);
                            cx.notify();
                        }
                    })
                })
            };
            let (s1, s2, paint, g2) = (set.clone(), set.clone(), self.paint.clone(), group.clone());
            let name = name_or(&f.name, "A friend");
            let status = if f.online {
                String::new()
            } else if f.last_seen > 0 {
                ago(f.last_seen)
            } else {
                "offline".into()
            };
            let head = face(&self.store, &f.mc_id, &name, rems(1.25), cx);
            rows.push(
                drag_pick::paint_row(h_flex().id(("group-pick", i)), &self.paint, group.clone(), move |v, w, cx| s1(v, w, cx))
                    .gap_2()
                    .px_2()
                    .py_1()
                    .rounded(theme.radius)
                    .hover(|s| widgets::row_hover(s, cx))
                    .child(
                        drag_pick::paint_option(div(), &self.paint, group.clone(), usize::from(!on), move |v, w, cx| s2(v, w, cx))
                            .child(Checkbox::new(("group-tick", i)).checked(on).on_click({
                                let set = set.clone();
                                move |v: &bool, w, cx| {
                                    if paint.active(&g2) {
                                        return;
                                    }
                                    set(usize::from(*v), w, cx)
                                }
                            })),
                    )
                    .child(head)
                    .child(div().flex_1().min_w_0().truncate().text_sm().child(name))
                    .when(!status.is_empty(), |r| r.child(widgets::caption(status, cx).text_xs()))
                    .into_any_element(),
            );
        }
        let end = self.paint.clone();
        v_flex()
            .id("group-pick-list")
            .w_full()
            .max_h(rems(14.))
            .overflow_y_scroll()
            .gap_0p5()
            .on_mouse_up(MouseButton::Left, move |_, _, _| end.end())
            .children(rows)
            .into_any_element()
    }
}

impl Render for GroupForm {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        use gpui_kit::component::form::Field;
        let rem = window.rem_size();
        let mut form = crate::java_form::settings_form(rem);
        if !matches!(self.mode, GroupMode::Add(_)) {
            form = form.child(Field::new().label("Name").child(Input::new(&self.name)));
        }
        if matches!(self.mode, GroupMode::Rename(_)) {
            return form.into_any_element();
        }
        let list = self.list(cx);
        form = form.child(Field::new().label("Friends").child(list));
        // The notes sit outside the form: its grid sizes a column to the
        // text's one-line width, so wrapped text there runs past the dialog.
        // Same label column (9 rem), gaps and label style as the form's rows.
        let notes = h_flex()
            .w_full()
            .gap_2()
            .items_center()
            .child(
                div()
                    .w(rems(9.))
                    .flex_none()
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM)
                    .child("How it works"),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_1()
                    .when(matches!(self.mode, GroupMode::New), |c| {
                        c.child(widgets::fine_print(
                            "A group shares packs with several friends at once. Turn on \u{201c}Keep my packs the same as the group\u{201d} and this PC follows every pack someone in the group follows.",
                            cx,
                        ))
                    })
                    .child(widgets::fine_print("Each friend gets an invitation. They're in the group once they say yes.", cx))
                    .child(widgets::fine_print(
                        "Everyone in the group gets the packs shared with it, including friends you add later.",
                        cx,
                    )),
            );
        v_flex().w_full().gap_3().child(form).child(notes).into_any_element()
    }
}

// ---- helpers -------------------------------------------------------------------

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// "5 min ago", "2 h ago", "3 days ago", "4 months ago".
fn ago(at: u64) -> String {
    let d = now().saturating_sub(at);
    let (n, unit) = match d {
        0..60 => return "just now".into(),
        60..3600 => (d / 60, "min"),
        3600..86400 => (d / 3600, "h"),
        86400..2_592_000 => (d / 86400, "day"),
        2_592_000..31_536_000 => (d / 2_592_000, "month"),
        _ => (d / 31_536_000, "year"),
    };
    let s = if n == 1 || unit == "min" || unit == "h" { "" } else { "s" };
    format!("{n} {unit}{s} ago")
}

/// How a followed copy is doing, for its row. Copies update by themselves;
/// only mods nobody could be asked about bring back the Update button.
#[derive(Clone, Debug, PartialEq)]
enum CopyState {
    UpToDate,
    /// A newer version is being applied, or waits for another task.
    Updating,
    /// The last try failed: why, and whether Hopper tries again by itself.
    Failed(String, bool),
    /// Mods from GitHub and other links left out until the player says.
    Held(Vec<String>),
}

impl CopyState {
    /// Which to show first: the ones that need the player.
    fn rank(&self) -> u8 {
        match self {
            CopyState::Failed(..) => 0,
            CopyState::Held(_) => 1,
            CopyState::Updating => 2,
            CopyState::UpToDate => 3,
        }
    }

    /// The line under the pack's name, when there's something to say.
    fn note(&self, sender: &str) -> Option<String> {
        match self {
            CopyState::Failed(why, true) => Some(format!("Couldn't get {sender}'s changes: {why} Trying again soon.")),
            CopyState::Failed(why, false) => Some(format!("Couldn't get {sender}'s changes: {why}")),
            CopyState::Held(names) => Some(match names.as_slice() {
                [one] => format!("{one} is from GitHub or another link, so it wasn't added. Update asks about it."),
                many => format!("{} mods from GitHub and other links weren't added. Update asks about them.", many.len()),
            }),
            _ => None,
        }
    }
}

fn copy_state(s: &Store, f: &hopper::settings::FollowedRecipe, root: &Path) -> CopyState {
    if let Some((why, again)) = s.follow_error(root) {
        return CopyState::Failed(why.to_string(), again);
    }
    if s.follow_updating(root) {
        return CopyState::Updating;
    }
    if !f.held.is_empty() {
        return CopyState::Held(f.held.clone());
    }
    CopyState::UpToDate
}

/// One followed copy and how it's doing: what the header's chip, the Packs
/// page's card and the rail's badges are made from.
struct SyncRow {
    root: PathBuf,
    /// The pack's name, as the friends' list shows it.
    name: String,
    /// Whose changes it gets: the sender, or "the group".
    who: String,
    /// The group it's kept in step with, if any.
    group: String,
    state: CopyState,
}

/// Every copy this PC still follows, the ones that need the player first.
fn sync_rows(s: &Store) -> Vec<SyncRow> {
    let mut rows: Vec<SyncRow> = s
        .settings()
        .followed_recipes
        .iter()
        .filter(|f| f.ended.is_empty())
        .map(|f| {
            let root = PathBuf::from(&f.root);
            let name = f.name.clone();
            let who = if s.group_copy(&root).is_some() { "the group".to_string() } else { name_or(&f.peer_name, "your friend") };
            SyncRow { state: copy_state(s, f, &root), root, name, who, group: f.group.clone() }
        })
        .collect();
    rows.sort_by_key(|r| r.state.rank());
    rows
}

/// The badge for a tile whose packs need the player: red when one failed,
/// amber when one waits for an answer.
fn alert_of<'a>(rows: impl Iterator<Item = &'a SyncRow>, cx: &App) -> Option<TileMark> {
    match rows.map(|r| r.state.rank()).min()? {
        0 => Some(TileMark::Alert(cx.theme().danger)),
        1 => Some(TileMark::Alert(cx.theme().warning)),
        _ => None,
    }
}

/// Who changed a group pack last, and how: "Mia turned off 2 mods · 3 h
/// ago". Changes this PC made aren't news, so they're skipped.
fn changed_by(s: &Store, root: &Path, me: &str) -> Option<String> {
    use hopper::groupsync::{LOADER_KEY, State};
    let c = s.group_copy(root)?;
    // What others did to a mod kept only on this PC didn't happen here.
    let kept = s.instance(root).map(|i| &i.only_here);
    // The loader counts while this instance is on the one the group chose
    // (it isn't when the group moved to another kind of loader).
    let loader = s.instance(root).map(|i| State::File { file: format!("loader:{}", i.loader), on: true });
    let marks = || {
        c.marks
            .iter()
            .filter(|(k, m)| if k.as_str() == LOADER_KEY { Some(&m.state) == loader.as_ref() } else { !kept.is_some_and(|o| o.contains_key(*k)) })
            .map(|(k, m)| (k.as_str() == LOADER_KEY, m))
    };
    let (is_loader, last) = marks().filter(|(_, m)| m.by != me).max_by_key(|(_, m)| (m.at, m.seq))?;
    // The mods that one change touched the same way.
    let n = marks().filter(|(l, m)| !l && m.by == last.by && m.seq == last.seq && m.kind == last.kind).count();
    let verb = match last.kind {
        Kind::Added => "added",
        Kind::Removed => "took out",
        Kind::On => "turned on",
        Kind::Off => "turned off",
        Kind::Updated => "updated",
    };
    let what = if is_loader {
        "the loader".to_string()
    } else if n == 1 {
        "a mod".to_string()
    } else {
        format!("{n} mods")
    };
    let when = if last.at > 0 { format!(" · {}", ago(last.at)) } else { String::new() };
    Some(format!("{} {verb} {what}{when}", name_or(&last.name, "Someone")))
}

/// A warning under a pack's name: a small triangle and a short sentence.
fn warn_line(text: String, cx: &App) -> Div {
    h_flex()
        .gap_1()
        .items_start()
        .pl(rems(1.625))
        .child(Icon::new(IconName::TriangleAlert).xsmall().flex_none().mt(px(2.)).text_color(cx.theme().warning))
        // Wraps instead of cutting off: the end says whether Hopper tries
        // again, which the player needs to read.
        .child(div().flex_1().min_w_0().text_xs().text_color(cx.theme().muted_foreground).child(text))
}

// ---- dragging friends onto groups, and Undo ------------------------------------

/// How long a take-out waits for Undo before it's sent: as long as its
/// toast stays up (gpui-kit hides it after 5 s), and a little more.
const UNDO_SECS: u64 = 6;

/// A friend dragged onto a group's tile, or out of a group's page.
#[derive(Clone)]
struct FriendDrag {
    id: String,
    name: SharedString,
    /// The group page they were dragged from, when this PC made that group:
    /// a plain drop moves them, Ctrl+drop keeps them there too.
    from_group: Option<String>,
    /// Where they can land and what that does, filled in as the panel draws.
    spots: Rc<RefCell<Vec<DropSpot>>>,
    /// Where in the row the pointer took hold: the drag is drawn that far
    /// up and left of the pointer, so the card moves it back beside it.
    grab: Point<Pixels>,
}

/// A place a dragged friend can land: its bounds, what dropping does
/// (adding, and moving when dragged out of a group), and whether it can.
#[derive(Clone)]
struct DropSpot {
    at: Bounds<Pixels>,
    add: SharedString,
    moved: SharedString,
    ok: bool,
}

impl Render for FriendDrag {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let at = window.mouse_position();
        let keep = window.modifiers().control;
        // What the spot under the pointer would do, said beside the name, as
        // a tooltip would if one could show while dragging.
        let hint = self.spots.borrow().iter().find(|s| s.at.contains(&at)).map(|s| {
            let text = if self.from_group.is_some() && !keep { s.moved.clone() } else { s.add.clone() };
            (text, s.ok)
        });
        let card = |d: Div| d.px_2p5().py_1().rounded(theme.radius).bg(theme.popover).border_1().border_color(theme.border).shadow_md();
        v_flex()
            .pl(self.grab.x + px(14.))
            .pt(self.grab.y + px(10.))
            // Drawn outside the window's tree: nothing to inherit a colour from.
            .text_color(theme.popover_foreground)
            .gap_1()
            .items_start()
            .child(card(div()).text_sm().font_medium().child(self.name.clone()))
            .when_some(hint, |c, (text, ok)| {
                c.child(
                    card(h_flex().gap_1p5())
                        .text_xs()
                        .text_color(if ok { theme.popover_foreground } else { theme.muted_foreground })
                        .when(!ok, |d| d.child(Icon::new(IconName::Lock).xsmall()))
                        .child(text),
                )
            })
    }
}

impl FriendsPanel {
    /// A drag (or a menu's move) of this friend, from `from_group`.
    fn drag_of(&self, id: &str, name: &str, from_group: Option<String>) -> FriendDrag {
        FriendDrag { id: id.to_string(), name: name.to_string().into(), from_group, spots: self.spots.clone(), grab: Point::default() }
    }

    /// Starts dragging a row: the rail lights up the groups it can go to.
    fn start_drag<E: StatefulInteractiveElement>(&self, el: E, drag: FriendDrag, cx: &mut Context<Self>) -> E {
        let view = cx.entity();
        el.on_drag(drag, move |d, grab, _, cx| {
            let d = FriendDrag { grab, ..d.clone() };
            let d2 = d.clone();
            view.update(cx, |this, cx| {
                this.dragging = Some(d2);
                cx.notify();
            });
            cx.new(|_| d)
        })
    }

    /// A take-out still waiting for Undo hides the member already.
    fn leaving(&self, gid: &str, id: &str) -> bool {
        self.pending_out.iter().any(|p| p.0 == gid && p.1 == id)
    }

    /// What dropping `d` on group `g` does: (adding, moving, allowed).
    /// When it isn't allowed, both say why.
    fn drop_text(&self, g: &Group, d: &FriendDrag, cx: &App) -> (String, String, bool) {
        let s = self.store.read(cx);
        let me = s.mailbox_id();
        let (who, gname) = (d.name.to_string(), g.name.clone());
        let no = |t: String| (t.clone(), t, false);
        if !g.me.joined {
            return no(format!("Join {gname} first"));
        }
        if g.owner != me {
            let owner = g.members.iter().find(|m| m.id == g.owner).map(|m| name_or(&m.name, "its maker")).unwrap_or_else(|| "its maker".into());
            return no(format!("Only {owner} can add people"));
        }
        if g.members.iter().any(|m| m.id == d.id) && !self.leaving(&g.id, &d.id) {
            return no(format!("{who} is already in {gname}"));
        }
        if !s.mailbox_friends().iter().any(|f| f.id == d.id) {
            return no(format!("{who} isn't your friend yet"));
        }
        if !matches!(s.sync_status(), SyncStatus::Ok) {
            return no(CANT_CONNECT.to_string());
        }
        (format!("Add {who} to {gname}"), format!("Move {who} to {gname}"), true)
    }

    /// Puts a friend in a group: they get an invite. A plain drop from a
    /// group page also takes them out of that group once Undo has passed.
    /// `keep`: Ctrl was held, or the menu's Add: they stay where they were.
    fn drop_on(&mut self, gid: &str, d: &FriendDrag, keep: bool, window: &mut Window, cx: &mut Context<Self>) {
        let s = self.store.read(cx);
        let Some(g) = s.mailbox_groups().iter().find(|g| g.id == gid).cloned() else { return };
        if !self.drop_text(&g, d, cx).2 {
            return;
        }
        let from = if keep { None } else { d.from_group.clone().filter(|f| f != gid) };
        let from_name = from.as_ref().and_then(|f| s.mailbox_groups().iter().find(|x| &x.id == f).map(|x| x.name.clone()));
        let (id, who) = (d.id.clone(), d.name.to_string());
        // Dropped back on a group they're leaving: they just stay.
        let stays = self.cancel_out(gid, &id);
        if !stays {
            self.store.update(cx, |s, cx| s.edit_members(gid, vec![id.clone()], Vec::new(), cx));
        }
        let token = from.as_ref().map(|f| self.take_out_later(f, &id, cx));
        let msg = match (&from_name, stays) {
            (_, true) => format!("{who} stays in {}.", g.name),
            (Some(f), _) => format!("{who} is invited to {} and out of {f}. They're in once they say yes.", g.name),
            (None, _) => format!("{who} is invited to {}. They're in once they say yes.", g.name),
        };
        let gid = gid.to_string();
        self.undo_toast(
            msg,
            move |this, cx| {
                if stays {
                    this.take_out_later(&gid, &id, cx);
                } else {
                    this.store.update(cx, |s, cx| s.edit_members(&gid, Vec::new(), vec![id.clone()], cx));
                }
                if let (Some(f), Some(t)) = (&from, token) {
                    // Too late to stop the take-out: invite them back.
                    if !this.cancel_token(t) {
                        this.store.update(cx, |s, cx| s.edit_members(f, vec![id.clone()], Vec::new(), cx));
                    }
                }
            },
            window,
            cx,
        );
    }

    /// Takes `d` out of the group whose page they were dragged off.
    fn take_out_now(&mut self, gid: &str, d: &FriendDrag, window: &mut Window, cx: &mut Context<Self>) {
        let Some(gname) = self.store.read(cx).mailbox_groups().iter().find(|g| g.id == gid).map(|g| g.name.clone()) else { return };
        let (gid, id) = (gid.to_string(), d.id.clone());
        let token = self.take_out_later(&gid, &id, cx);
        self.undo_toast(
            format!("{} is out of {gname}.", d.name),
            move |this, cx| {
                if !this.cancel_token(token) {
                    this.store.update(cx, |s, cx| s.edit_members(&gid, vec![id.clone()], Vec::new(), cx));
                }
            },
            window,
            cx,
        );
    }

    /// Takes `id` out of `gid` after Undo's time, unless undone first.
    fn take_out_later(&mut self, gid: &str, id: &str, cx: &mut Context<Self>) -> u64 {
        self.out_token += 1;
        let token = self.out_token;
        self.pending_out.push((gid.to_string(), id.to_string(), token));
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs(UNDO_SECS)).await;
            this.update(cx, |this, cx| {
                if let Some(i) = this.pending_out.iter().position(|p| p.2 == token) {
                    let (gid, id, _) = this.pending_out.remove(i);
                    this.store.update(cx, |s, cx| s.edit_members(&gid, Vec::new(), vec![id], cx));
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
        cx.notify();
        token
    }

    /// Stops a waiting take-out; false once it has been sent.
    fn cancel_token(&mut self, token: u64) -> bool {
        let before = self.pending_out.len();
        self.pending_out.retain(|p| p.2 != token);
        before != self.pending_out.len()
    }

    /// Stops a waiting take-out of `id` from `gid`; false when none waits.
    fn cancel_out(&mut self, gid: &str, id: &str) -> bool {
        let before = self.pending_out.len();
        self.pending_out.retain(|p| !(p.0 == gid && p.1 == id));
        before != self.pending_out.len()
    }

    /// A toast saying what just happened, with Undo.
    fn undo_toast(&mut self, msg: String, undo: impl Fn(&mut Self, &mut Context<Self>) + 'static, window: &mut Window, cx: &mut Context<Self>) {
        let view = cx.entity();
        let done = Rc::new(std::cell::Cell::new(false));
        let undo = Rc::new(undo);
        let note = Notification::success(msg)
            .action(move |_, _, cx| {
                let (view, done, undo) = (view.clone(), done.clone(), undo.clone());
                Button::new("friends-undo").label("Undo").disabled(done.get()).on_click(cx.listener(move |note, _, window, cx| {
                    if !done.replace(true) {
                        view.update(cx, |this, cx| undo(this, cx));
                    }
                    note.dismiss(window, cx);
                }))
            })
            .autohide(true);
        window.push_notification(note, cx);
    }

    /// Taking someone out from a menu asks first, with a tick box to take
    /// them out of this PC's other groups too. No typing.
    fn confirm_take_out(&mut self, g: &Group, id: String, who: String, window: &Window, cx: &mut Context<Self>) {
        let s = self.store.read(cx);
        let me = s.mailbox_id();
        let (others, other_names): (Vec<String>, Vec<String>) = s
            .mailbox_groups()
            .iter()
            .filter(|x| x.owner == me && x.id != g.id && x.members.iter().any(|m| m.id == id) && !self.leaving(&x.id, &id))
            .map(|x| (x.id.clone(), x.name.clone()))
            .unzip();
        // One other group is named; more are counted.
        let also = match other_names.as_slice() {
            [one] => format!("Also take {who} out of {one}"),
            _ => format!("Also take {who} out of your {} other groups", other_names.len()),
        };
        let (gid, gname) = (g.id.clone(), g.name.clone());
        let handle = window.window_handle();
        let store = self.store.clone();
        cx.defer(move |cx| {
            handle
                .update(cx, |_, window, cx| {
                    let every = Rc::new(std::cell::Cell::new(false));
                    window.open_dialog(cx, move |dialog, window, _| {
                        let n = others.len();
                        let all = every.get();
                        let tick = every.clone();
                        let (store, gid, id, others2, every2) = (store.clone(), gid.clone(), id.clone(), others.clone(), every.clone());
                        dialog
                            .title(format!("Take {who} out of {gname}?"))
                            .w(rems(28.).to_pixels(window.rem_size()))
                            .margin_top(rems(widgets::DIALOG_TOP).to_pixels(window.rem_size()))
                            .child(
                                v_flex()
                                    .gap_3()
                                    .child(div().text_sm().child(if all {
                                        format!("{who} stops getting changes to packs they got only from these groups. Their copies stay on their PC.")
                                    } else {
                                        format!("{who} stops getting changes to packs they got only from {gname}. Their copies stay on their PC.")
                                    }))
                                    .when(n > 0, |c| {
                                        c.child(
                                            Checkbox::new("take-out-all")
                                                .label(also.clone())
                                                .checked(all)
                                                .on_change(move |v: &bool, window, _| {
                                                    tick.set(*v);
                                                    window.refresh();
                                                }),
                                        )
                                    }),
                            )
                            .footer(
                                DialogFooter::new()
                                    .child(DialogClose::new().child(Button::new("dialog-cancel").outline().label("Cancel")))
                                    .child(DialogAction::new().child(Button::new("dialog-ok").danger().label(if all {
                                        format!("Take out of {} groups", n + 1)
                                    } else {
                                        "Take out".to_string()
                                    }))),
                            )
                            .on_ok(move |_, _, cx| {
                                let all = every2.get();
                                store.update(cx, |s, cx| {
                                    s.remove_from_group(&gid, vec![id.clone()], cx);
                                    if all {
                                        for o in &others2 {
                                            s.remove_from_group(o, vec![id.clone()], cx);
                                        }
                                    }
                                });
                                true
                            })
                    });
                })
                .ok();
        });
    }

    /// A member's ⋯ on a group page: move or add them to another group,
    /// or take them out. The keyboard's way to do what dragging does.
    fn member_menu(&self, id: SharedString, g: &Group, d: FriendDrag, down: bool, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let mine = d.from_group.is_some();
        let moves = if mine { self.group_choices(&d, true, cx) } else { Vec::new() };
        let adds = self.group_choices(&d, false, cx);
        let view = cx.entity();
        let (g2, who) = (g.clone(), d.name.to_string());
        Button::new(id)
            .ghost()
            .xsmall()
            .icon(IconName::Ellipsis)
            .tooltip("More")
            .disabled(down)
            .dropdown_menu_with_anchor(Anchor::TopRight, move |mut menu, window, cx| {
                if !moves.is_empty() {
                    menu = group_submenu(menu, "Move to", &view, &moves, &d, false, window, cx);
                }
                if !adds.is_empty() {
                    menu = group_submenu(menu, "Add to group", &view, &adds, &d, true, window, cx);
                }
                if mine {
                    let (v, g, id, who) = (view.clone(), g2.clone(), d.id.clone(), who.clone());
                    menu = menu.separator().item(PopupMenuItem::new(format!("Take {who} out…")).icon(IconName::UserMinus).on_click(move |_, window, cx| {
                        v.update(cx, |t, cx| t.confirm_take_out(&g, id.clone(), who.clone(), window, cx))
                    }));
                }
                menu
            })
    }

    /// While a member of this PC's group is dragged, the top of its page
    /// takes them out when they're dropped there.
    fn take_out_zone(&self, g: &Group, cx: &mut Context<Self>) -> Option<AnyElement> {
        let d = self.dragging.clone().filter(|d| d.from_group.as_deref() == Some(g.id.as_str()))?;
        let theme = cx.theme().clone();
        // The theme's red is too dark to read on a dark panel (3.8:1); lifted, it reads at 4.5:1 or more.
        let red = if theme.mode.is_dark() {
            let c = theme.danger;
            hsla(c.h, c.s, c.l.max(0.68), 1.)
        } else {
            crate::theme::ink(theme.danger)
        };
        let gid = g.id.clone();
        Some(
            div()
                .id("take-out-zone")
                .relative()
                .mx_3()
                .mt_2()
                .px_3()
                .py_2()
                .rounded(theme.radius_lg)
                .border_1()
                .border_dashed()
                .border_color(theme.danger)
                .text_sm()
                .text_color(red)
                .flex()
                .items_center()
                .gap_2()
                .child(Icon::new(IconName::UserMinus).small())
                .child(format!("Drop here to take {} out of the group", d.name))
                .drag_over::<FriendDrag>(move |s, _, _, _| s.bg(theme.danger.opacity(widgets::alpha::TINT)))
                .on_drop(cx.listener(move |this, d: &FriendDrag, window, cx| this.take_out_now(&gid, d, window, cx)))
                .into_any_element(),
        )
    }

    /// A rail tile a dragged friend can land on: dimmed when they can't,
    /// tinted under the pointer when they can. `act` runs on the drop. It
    /// takes drops before the panel has drawn the drag (a quick flick),
    /// so `act` checks again.
    fn drop_target(
        &self,
        el: Stateful<Div>,
        texts: Option<(String, String, bool)>,
        act: impl Fn(&mut Self, &FriendDrag, &mut Window, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let theme = cx.theme().clone();
        let ok = texts.as_ref().is_none_or(|t| t.2);
        let el = el
            .when(ok, |d| d.drag_over::<FriendDrag>(move |s, _, _, _| s.bg(theme.primary.opacity(widgets::alpha::SELECTED))))
            .on_drop(cx.listener(move |this, d: &FriendDrag, window, cx| act(this, d, window, cx)));
        let Some((add, moved, ok)) = texts else { return el };
        let spots = self.spots.clone();
        let (add, moved): (SharedString, SharedString) = (add.into(), moved.into());
        el.relative().when(!ok, |d| d.opacity(0.4)).child(
            canvas(
                move |b, _, _| spots.borrow_mut().push(DropSpot { at: b, add: add.clone(), moved: moved.clone(), ok }),
                |_, _, _, _| {},
            )
            .absolute()
            .top_0()
            .left_0()
            .size_full(),
        )
    }

    /// The groups a friend can be added to or moved to, for a menu:
    /// (group, label, allowed). The ones that can't take them say why.
    fn group_choices(&self, d: &FriendDrag, moving: bool, cx: &App) -> Vec<(String, String, bool)> {
        let s = self.store.read(cx);
        s.mailbox_groups()
            .iter()
            .filter(|g| g.me.joined && d.from_group.as_deref() != Some(g.id.as_str()))
            .map(|g| {
                let (add, moved, ok) = self.drop_text(g, d, cx);
                let label = if ok { g.name.clone() } else { format!("{} · {}", g.name, if moving { moved } else { add }) };
                (g.id.clone(), label, ok)
            })
            .collect()
    }
}

/// "Add to group" / "Move to": one item per group, as dropping on its tile.
#[allow(clippy::too_many_arguments)]
fn group_submenu(
    menu: PopupMenu,
    label: &str,
    view: &Entity<FriendsPanel>,
    choices: &[(String, String, bool)],
    drag: &FriendDrag,
    keep: bool,
    window: &mut Window,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    let (view, choices, drag) = (view.clone(), choices.to_vec(), drag.clone());
    menu.submenu_with_icon(Some(Icon::new(IconName::Users)), label.to_string(), window, cx, move |mut sub, _, _| {
        for (gid, label, ok) in &choices {
            let (v, gid, d) = (view.clone(), gid.clone(), drag.clone());
            sub = sub.item(PopupMenuItem::new(label.clone()).disabled(!ok).on_click(move |_, window, cx| {
                v.update(cx, |t, cx| t.drop_on(&gid, &d, keep, window, cx))
            }));
        }
        sub
    })
}

fn name_or(name: &str, or: &str) -> String {
    if name.trim().is_empty() { or.to_string() } else { name.to_string() }
}

/// A friend's pack's icon and CurseForge project, when an instance here is
/// the same pack (the code names the pack; its icon isn't sent).
fn pack_look(s: &Store, code: &str) -> (Option<PathBuf>, u64) {
    let Ok(share) = hopper::sharecode::decode(code) else { return (None, 0) };
    let hopper::sharecode::Base::Curse { project, .. } = share.base else { return (None, 0) };
    let icon = s
        .instances()
        .iter()
        .find(|i| i.pack.as_ref().is_some_and(|p| p.project_id == project))
        .and_then(|i| i.icon.as_deref())
        .and_then(|u| s.image(u))
        .map(Path::to_path_buf);
    (icon, project)
}

#[cfg(test)]
mod tests {
    use super::*;

    // `super::*` brings in gpui's `test` attribute, which expands to `#[test]`
    // again and loops; name the standard one.
    #[::core::prelude::v1::test]
    fn ago_reads_like_a_person() {
        let t = now();
        assert_eq!(ago(t), "just now");
        assert_eq!(ago(t - 5 * 60), "5 min ago");
        assert_eq!(ago(t - 2 * 3600), "2 h ago");
        assert_eq!(ago(t - 86400), "1 day ago");
        assert_eq!(ago(t - 3 * 86400), "3 days ago");
        assert_eq!(ago(t - 4 * 2_592_000), "4 months ago");
    }

    #[::core::prelude::v1::test]
    fn group_initials_read_like_discords() {
        assert_eq!(initials("drunk gamblers"), "DG");
        assert_eq!(initials("The Late Night Survival Crew"), "TL");
        assert_eq!(initials("Builders"), "B");
        assert_eq!(initials("  ünder  -dogs "), "ÜD");
        assert_eq!(initials("!!"), "?");
    }

    /// Every string the Friends panel can show: its string literals, with
    /// comments skipped.
    fn panel_strings() -> Vec<String> {
        let src: Vec<char> = include_str!("friends.rs").chars().collect();
        let (mut out, mut i) = (Vec::new(), 0);
        while i < src.len() {
            match src[i] {
                '/' if src.get(i + 1) == Some(&'/') => {
                    while i < src.len() && src[i] != '\n' {
                        i += 1;
                    }
                }
                // A char literal ('"' or '\\'), not a lifetime.
                '\'' if src.get(i + 1) == Some(&'\\') => i += 3,
                '\'' if src.get(i + 2) == Some(&'\'') => i += 2,
                '"' => {
                    let mut lit = String::new();
                    i += 1;
                    while i < src.len() && src[i] != '"' {
                        if src[i] == '\\' {
                            i += 1;
                        } else {
                            lit.push(src[i]);
                        }
                        i += 1;
                    }
                    out.push(lit);
                }
                _ => {}
            }
            i += 1;
        }
        out
    }

    // The same words `players_never_see_the_services_own_words` keeps out of
    // the service's errors stay out of the panel's own text.
    #[::core::prelude::v1::test]
    fn the_panel_never_names_the_service() {
        let strings = panel_strings();
        assert!(strings.iter().any(|s| s == "Open a friend to see the packs they share with you."));
        assert!(strings.iter().any(|s| s == "Invite a friend, or join a friend with their code"));
        let words = ["relay", "mailbox", "sync server", "sync service", "token", "pair", "peer"];
        for s in &strings {
            // This test's own list.
            if words.contains(&s.as_str()) {
                continue;
            }
            let lower = s.to_lowercase();
            assert!(!words.iter().any(|w| lower.contains(w)), "{s}");
        }
    }
}
