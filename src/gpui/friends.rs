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
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::radio::Radio;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, Selectable as _, Sizable as _, StyledExt as _, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
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

/// The tabs.
const FRIENDS: usize = 0;
const YOUR_PACKS: usize = 1;

/// A friend sharing more packs than this gets a search box on their page.
const PAGE_SEARCH_FROM: usize = 8;

/// What the Friends tab shows: the list, or one friend's, group's or the
/// stopped follows' page in its place.
#[derive(Clone, PartialEq)]
enum Page {
    List,
    Friend(String),
    Group(String),
    Ended,
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
    tab: usize,
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
    /// Where the + was last drawn, for its popover.
    plus_anchor: Rc<RefCell<Option<Bounds<Pixels>>>>,
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
            tab: FRIENDS,
            join_code,
            share_pop: None,
            anchors: Rc::default(),
            paint: drag_pick::Paint::default(),
            ticking: false,
            page: Page::List,
            page_since: 0,
            plus_pop: false,
            plus_anchor: Rc::default(),
            search,
            pack_search,
            by_name: false,
            invite_hidden: false,
            pack_search_for: String::new(),
            focus: cx.focus_handle(),
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
            self.page = Page::List;
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
        if page != Page::List {
            window.focus(&self.focus, cx);
        }
        self.tab = FRIENDS;
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

    /// The + popover: invite a friend, join with their code, new group.
    fn plus_overlay(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let anchor = (*self.plus_anchor.borrow())?;
        let theme = cx.theme().clone();
        let down = matches!(self.store.read(cx).sync_status(), SyncStatus::Down(_));
        let can_group = self.can_group(cx);
        let place = widgets::anchored_popover(anchor, window);
        let me = cx.entity();
        let invite = self.invite_block(down, true, cx);
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
                // The + toggles it itself.
                if anchor.contains(&e.position) {
                    return;
                }
                me.update(cx, |t, cx| t.set_plus(false, cx))
            })
            .child(invite)
            .child(or_line(cx))
            .child(join)
            .when(can_group, |c| {
                c.child(div().h_px().bg(theme.border)).child(
                    h_flex()
                        .id("plus-new-group")
                        .gap_2()
                        .px_2()
                        .py_1()
                        .rounded(theme.radius)
                        .text_sm()
                        .clickable(cx)
                        .hover(|s| widgets::row_hover(s, cx))
                        .child(Icon::new(IconName::Users).small().text_color(theme.muted_foreground))
                        .child("New group\u{2026}")
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.set_plus(false, cx);
                            this.group_dialog(GroupMode::New, None, window, cx)
                        })),
                )
            });
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

    fn header(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        h_flex()
            .flex_none()
            .h(rems(3.))
            .px_3()
            .gap_2()
            .child(div().flex_1().text_base().font_semibold().child("Friends"))
            .when(self.store.read(cx).sync_status() != SyncStatus::Off, |h| {
                let anchor = self.plus_anchor.clone();
                h.child(
                    div()
                        .relative()
                        .flex_none()
                        .child(
                            Button::new("friends-plus")
                                .ghost()
                                .small()
                                .icon(IconName::Plus)
                                .selected(self.plus_pop)
                                .tooltip("Invite a friend, join with a code, or make a group")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    let open = !this.plus_pop;
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

    fn tabs(&self, friends: Option<usize>, shared: usize, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let me = cx.entity();
        h_flex().flex_none().px_2().border_b_1().border_color(cx.theme().border).child(widgets::underline_tabs(
            "friends-tab",
            vec![("Friends".into(), friends), ("Your packs".into(), Some(shared))],
            self.tab,
            move |ix, _, cx| {
                me.update(cx, |t, cx| {
                    t.tab = *ix;
                    t.set_pop(None, cx);
                })
            },
            cx,
        ))
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

    /// No friends and nothing shared with this PC yet: what Friends does,
    /// with the invite and join right there.
    fn empty(&mut self, down: bool, cx: &mut Context<Self>) -> AnyElement {
        v_flex()
            .w_full()
            .child(
                widgets::empty_state(
                    IconName::Users,
                    "Play the same packs as your friends",
                    None,
                    cx,
                )
                .child(div().px_4().child(how_it_works(cx)))
                .pb_4(),
            )
            .child(v_flex().px_4().gap_3().child(self.invite_block(down, false, cx)).child(or_line(cx)).child(self.join_block(down, cx)).child(widgets::fine_print(
                "Nothing to sign up for. Hopper only sends your friend codes and mod lists. Your worlds and other files stay on your PC.",
                cx,
            )))
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
                        this.tab = YOUR_PACKS;
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
        let s = self.store.read(cx);
        let me = s.mailbox_id();
        let add_to: Vec<(String, String)> = s
            .mailbox_groups()
            .iter()
            .filter(|g| g.owner == me && g.me.joined && !g.members.iter().any(|m| m.id == f.id))
            .map(|g| (g.id.clone(), g.name.clone()))
            .collect();
        let can_group = self.can_group(cx);
        let view = cx.entity();
        let first = name.to_string();
        Button::new(id)
            .ghost()
            .xsmall()
            .icon(IconName::Ellipsis)
            .tooltip("More")
            .dropdown_menu_with_anchor(Anchor::TopRight, move |menu, _, _| {
                let (v1, v2, f2) = (view.clone(), view.clone(), f.clone());
                let mut menu = menu.item(PopupMenuItem::new(format!("Choose packs to share with {first}")).icon(IconName::Share2).on_click(move |_, _, cx| {
                    v1.update(cx, |t, cx| {
                        t.tab = YOUR_PACKS;
                        cx.notify();
                    })
                }));
                // Groups this PC made that they aren't in yet, then a new one.
                for (gid, gname) in &add_to {
                    let (v, gid, fid) = (view.clone(), gid.clone(), f.id.clone());
                    menu = menu.item(PopupMenuItem::new(format!("Add {first} to {gname}")).icon(IconName::Users).on_click(move |_, _, cx| {
                        let (gid, fid) = (gid.clone(), fid.clone());
                        v.update(cx, |t, cx| t.store.update(cx, |s, cx| s.add_to_group(&gid, vec![fid], cx)))
                    }));
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
        h_flex()
            .id(row_id.clone())
            .group(row_id)
            .gap_2()
            .px_3()
            .py_1p5()
            .clickable(cx)
            .hover(|s| widgets::row_hover(s, cx))
            .on_click(cx.listener(move |this, _, window, cx| this.open(Page::Friend(id.clone()), window, cx)))
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
                        .label("All friends")
                        .on_click(cx.listener(|this, _, window, cx| this.open(Page::List, window, cx))),
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
            Some((_, local, CopyState::Failed(..))) => format!("Your copy: {local}"),
            Some((_, local, _)) => format!("Your copy: {local} · up to date"),
            None if e.at > 0 => format!("Updated {}", ago(e.at)),
            None => String::new(),
        };
        let waiting = match &shown {
            Some((root, _, CopyState::Held(_))) => Some(root.clone()),
            _ => None,
        };
        let copy_note = shown.as_ref().and_then(|f| f.2.note(&who));
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
                    _ => format!("From {sender} · up to date"),
                };
                let note = state.note(&who);
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

    /// The list's last row: the follows that stopped, opening their page.
    fn ended_row(&self, ended: &[(PathBuf, String, String, String)], cx: &mut Context<Self>) -> Option<AnyElement> {
        let first = ended.first()?;
        let theme = cx.theme().clone();
        let sub = match ended.len() {
            1 => format!("1 pack: {}", first.1),
            n => format!("{n} packs"),
        };
        Some(
            div()
                .mt_1p5()
                .border_t_1()
                .border_color(theme.border)
                .child(
                    h_flex()
                        .id("ended-row")
                        .gap_2()
                        .px_3()
                        .py_1p5()
                        .clickable(cx)
                        .hover(|s| widgets::row_hover(s, cx))
                        .on_click(cx.listener(|this, _, window, cx| this.open(Page::Ended, window, cx)))
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .child(widgets::caption("These won't change any more", cx).text_xs().font_medium())
                                .child(widgets::caption(sub, cx).text_xs().truncate()),
                        )
                        .child(Icon::new(IconName::ChevronRight).small().flex_none().text_color(theme.muted_foreground)),
                )
                .into_any_element(),
        )
    }

    /// The stopped follows' page; each can be taken off the list, and the
    /// instance stays.
    fn ended_page(&self, ended: Vec<(PathBuf, String, String, String)>, cx: &mut Context<Self>) -> (Option<AnyElement>, AnyElement) {
        let top = self.page_top(div().px_3().pt_1p5().pb_2p5().text_base().font_semibold().child("These won't change any more"), cx);
        let body = v_flex()
            .py_1()
            .children(ended.into_iter().enumerate().map(|(i, (root, pack, local, who))| {
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
            }))
            .child(div().px_3().py_2().child(widgets::fine_print(
                "The friend removed you, or stopped sharing the pack. Your copies stay on your PC as they are.",
                cx,
            )))
            .into_any_element();
        (Some(top), body)
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
                    .label("Decline")
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
        let others: Vec<_> = g.members.iter().filter(|m| m.id != me).cloned().collect();
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

    /// A group this PC is in, in the list: opens its page.
    fn group_row(&self, gi: usize, g: &Group, recipes: &[Event], words: &[String], cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let (faces, sub) = self.group_look(g, recipes, rems(1.25), cx);
        let id = g.id.clone();
        h_flex()
            .id(("group", gi))
            .gap_2()
            .px_3()
            .py_1p5()
            .clickable(cx)
            .hover(|s| widgets::row_hover(s, cx))
            .on_click(cx.listener(move |this, _, window, cx| this.open(Page::Group(id.clone()), window, cx)))
            .child(faces)
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(div().text_sm().font_medium().truncate().child(widgets::marked(g.name.clone(), words, cx)))
                    .child(widgets::caption(sub, cx).text_xs().truncate()),
            )
            .child(Icon::new(IconName::ChevronRight).small().flex_none().text_color(theme.muted_foreground))
            .into_any_element()
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

    /// A group's page: its members, "Keep my packs the same as the group",
    /// the packs it shares and the ones this PC shares with it.
    fn group_page(&mut self, g: &Group, recipes: &[Event], down: bool, window: &mut Window, cx: &mut Context<Self>) -> (Option<AnyElement>, AnyElement) {
        let theme = cx.theme().clone();
        let (faces, sub) = self.group_look(g, recipes, rems(1.5), cx);
        let menu = self.group_menu(g, cx);
        let who = h_flex()
            .gap_2()
            .px_3()
            .pt_1p5()
            .pb_2p5()
            .child(faces)
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(div().text_base().font_semibold().truncate().child(g.name.clone()))
                    .child(widgets::caption(sub, cx).text_xs().truncate()),
            )
            .child(menu);
        let top = self.page_top(who, cx);
        let s = self.store.read(cx);
        let me = s.mailbox_id();
        let mine = g.owner == me;
        let others: Vec<_> = g.members.iter().filter(|m| m.id != me).cloned().collect();
        let my_packs: Vec<(PathBuf, String)> = s
            .instances()
            .iter()
            .filter(|i| !i.id.is_empty() && s.settings().shares_with_group(&i.id, &g.id))
            .map(|i| (i.root.clone(), i.name.clone()))
            .collect();
        let can_add = mine && s.mailbox_friends().iter().any(|f| !g.members.iter().any(|m| m.id == f.id));
        // Members, the owner first.
        let mut members = others.clone();
        members.sort_by_key(|m| m.id != g.owner);
        let member_rows: Vec<AnyElement> = members
            .iter()
            .enumerate()
            .map(|(mi, m)| {
                let name = name_or(&m.name, "A friend");
                let status = if !m.joined {
                    "Not joined yet".to_string()
                } else if m.online && !down {
                    "Online".to_string()
                } else if m.last_seen > 0 {
                    format!("Last online {}", ago(m.last_seen))
                } else {
                    "Offline".to_string()
                };
                let status = if m.id == g.owner { format!("Created the group · {status}") } else { status };
                let f = face(&self.store, &m.mc_id, &name, rems(1.25), cx);
                let row_id = SharedString::from(format!("gm:{mi}"));
                let remove = (mine && m.id != g.owner).then(|| {
                    let (id, mid, gname, who) = (g.id.clone(), m.id.clone(), g.name.clone(), name.clone());
                    let b = Button::new(SharedString::from(format!("gm-x:{mi}")))
                        .xsmall()
                        .ghost()
                        .icon(IconName::Close)
                        .tooltip(format!("Take {who} out of {gname}"))
                        .disabled(down)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            let (id, mid) = (id.clone(), mid.clone());
                            this.confirm_group(
                                format!("Take {who} out of {gname}?"),
                                format!("{who} stops getting changes to packs they got only from {gname}. Their copies stay on their PC."),
                                "Take out",
                                Rc::new(move |s, cx| s.remove_from_group(&id, vec![mid.clone()], cx)),
                                window,
                                cx,
                            )
                        }));
                    widgets::reveal_on_hover(row_id.clone(), SharedString::from(format!("gm-x-at:{mi}")), b, window, cx)
                });
                h_flex()
                    .id(row_id.clone())
                    .group(row_id.clone())
                    .gap_2()
                    .px_3()
                    .py_0p5()
                    .hover(|s| widgets::row_hover(s, cx))
                    .child(f)
                    .child(div().flex_1().min_w_0().truncate().text_sm().child(name))
                    .child(widgets::caption(status, cx).text_xs().flex_none())
                    .children(remove)
                    .into_any_element()
            })
            .collect();
        // Packs others share with the group, as the friends' pack rows. The
        // feed says which groups each came through; the group's own list is
        // read less often.
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
                    .py_0p5()
                    .hover(|s| widgets::row_hover(s, cx))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_sm()
                            .clickable(cx)
                            .hover(|s| s.text_color(crate::theme::ink(theme.primary)))
                            .child(name.clone())
                            .id(SharedString::from(format!("gp-mine-name:{pi}")))
                            .on_click(cx.listener(move |_, _, _, cx| cx.emit(FriendsEvent::Show(root.clone())))),
                    )
                    .child(
                        Button::new(SharedString::from(format!("gp-change:{pi}")))
                            .xsmall()
                            .outline()
                            .label("Change")
                            .tooltip("Choose who gets this pack")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.tab = YOUR_PACKS;
                                cx.notify();
                            })),
                    )
                    .into_any_element()
            })
            .collect();
        let gid = g.id.clone();
        let body = v_flex()
            .pb_2()
            .child(cap("Members", Some(member_rows.len()), None, cx))
            .children(member_rows)
            .when(can_add, |c| {
                c.child(
                    h_flex().px_3().py_0p5().child(
                        Button::new("group-add")
                            .xsmall()
                            .ghost()
                            .icon(IconName::Plus)
                            .label("Add a friend")
                            .disabled(down)
                            .tooltip(if down { CANT_CONNECT } else { "Invite another friend into this group" })
                            .on_click(cx.listener(move |this, _, window, cx| this.group_dialog(GroupMode::Add(gid.clone()), None, window, cx))),
                    ),
                )
            })
            .child(
                h_flex()
                    .gap_3()
                    .items_start()
                    .px_3()
                    .pt_3()
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(div().text_sm().font_medium().child("Keep my packs the same as the group"))
                            .child(widgets::fine_print(
                                "When someone in the group follows one of its packs, this PC gets it too, as a new instance. When anyone turns a mod off or on, adds, removes or updates one in a group pack, everyone's copy changes too. Your other instances don't change. Each friend chooses this on their own PC.",
                                cx,
                            )),
                    )
                    .child(crate::switch::Switch::new("group-step").checked(in_step).on_click({
                        let store = self.store.clone();
                        move |v: &bool, _, cx| {
                            let (id, v) = (step_id.clone(), *v);
                            store.update(cx, |s, cx| s.set_in_step(&id, v, cx))
                        }
                    })),
            )
            .when(!pack_rows.is_empty(), |c| c.child(cap("Packs from the group", Some(pack_rows.len()), None, cx)).children(pack_rows))
            .child(cap("Your packs in the group", (!my_rows.is_empty()).then_some(my_rows.len()), None, cx))
            .when(my_rows.is_empty(), |c| {
                c.child(div().px_3().child(widgets::fine_print("You haven't shared any packs with the group. Pick some under Your packs.", cx)))
            })
            .children(my_rows)
            .into_any_element();
        (Some(top), body)
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

    /// The Friends tab: the list, or the open page (its top stays put
    /// above the scrolling part).
    fn friends_tab(&mut self, down: bool, window: &mut Window, cx: &mut Context<Self>) -> (Option<AnyElement>, AnyElement) {
        let s = self.store.read(cx);
        let friends = s.mailbox_friends().to_vec();
        let all_recipes = s.mailbox_recipes().to_vec();
        let recipes: Vec<Event> = all_recipes.iter().filter(|e| !s.pack_hidden(e)).cloned().collect();
        let groups: Vec<Group> = s.mailbox_groups().to_vec();
        let ended = self.ended_list(&friends, &all_recipes, cx);
        if self.page == Page::Ended && !ended.is_empty() {
            return self.ended_page(ended, cx);
        }
        if friends.is_empty() && all_recipes.is_empty() {
            self.page = Page::List;
            let row = self.ended_row(&ended, cx);
            return (None, v_flex().children(row).child(self.empty(down, cx)).into_any_element());
        }
        let people = self.people(&recipes, &groups, down, cx);
        match self.page.clone() {
            Page::Friend(id) => {
                if let Some(p) = people.iter().find(|p| p.id == id) {
                    return self.friend_page(p, down, window, cx);
                }
            }
            Page::Group(id) => {
                if let Some(g) = groups.iter().find(|g| g.id == id && g.me.joined) {
                    return self.group_page(g, &all_recipes, down, window, cx);
                }
            }
            Page::Ended | Page::List => {}
        }
        // The page's friend or group is gone: the list again.
        self.page = Page::List;
        (None, self.list(&people, &groups, &recipes, &all_recipes, &ended, down, window, cx))
    }

    /// The list: what this PC follows, groups, friends, and the follows
    /// that stopped. Searching narrows each and adds the packs that match.
    #[allow(clippy::too_many_arguments)]
    fn list(
        &mut self,
        people: &[Person],
        groups: &[Group],
        recipes: &[Event],
        all_recipes: &[Event],
        ended: &[(PathBuf, String, String, String)],
        down: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let s = self.store.read(cx);
        let has_friends = people.iter().any(|p| p.friend.is_some());
        let prompt = has_friends && s.settings().shared_with.is_empty() && !s.settings().share_prompt_done;
        let q = self.search.read(cx).value().to_string();
        let words = widgets::search_words(&q);
        let n_packs: usize = people.iter().map(|p| p.packs.len()).sum();
        let searchable = people.len() > 4 || n_packs > 12 || !words.is_empty();
        let reminder = self.invite_reminder(cx);
        let following = self.following(recipes, &words, down, cx);
        // Invitations first: they wait for an answer.
        let mut order: Vec<&Group> = groups.iter().filter(|g| !g.me.joined).collect();
        order.extend(groups.iter().filter(|g| g.me.joined));
        let group_rows: Vec<AnyElement> = order
            .into_iter()
            .filter(|g| hits(&g.name, &words))
            .enumerate()
            .map(|(gi, g)| if g.me.joined { self.group_row(gi, g, all_recipes, &words, cx) } else { self.invitation(gi, g, down, cx) })
            .collect();
        let friend_rows: Vec<AnyElement> = people
            .iter()
            .enumerate()
            .filter(|(_, p)| hits(&p.name, &words))
            .map(|(i, p)| self.friend_row(i, p, &words, down, window, cx))
            .collect();
        // Searching: the packs that match, from whoever shares them.
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
        let ended_row = if words.is_empty() { self.ended_row(ended, cx) } else { None };
        let nothing = !words.is_empty() && following.is_empty() && group_rows.is_empty() && friend_rows.is_empty() && pack_hits.is_empty();
        let hint = words.is_empty() && people.len() == 1 && !people[0].packs.is_empty();
        let n_groups = group_rows.len();
        v_flex()
            .pb_2()
            .when(prompt, |c| c.child(div().px_3().pt_3().child(self.prompt(cx))))
            .children(reminder)
            .when(searchable, |c| c.child(search_box(&self.search)))
            .when(!following.is_empty(), |c| c.child(cap("Following", Some(following.len()), None, cx)).children(following))
            .when(n_groups > 0, |c| c.child(cap("Groups", Some(n_groups), None, cx)).children(group_rows))
            .when(!friend_rows.is_empty(), |c| c.child(cap("Friends", Some(friend_rows.len()), None, cx)).children(friend_rows))
            .when(hint, |c| c.child(div().px_3().py_2().child(widgets::fine_print("Open a friend to see the packs they share with you.", cx))))
            .when(!pack_hits.is_empty(), |c| c.child(cap("Packs", Some(pack_hits.len()), None, cx)).children(pack_hits))
            .when(nothing, |c| c.child(div().px_3().py_3().child(widgets::fine_print(format!("Nothing matches \u{201c}{q}\u{201d}."), cx))))
            .children(ended_row)
            .into_any_element()
    }

    fn packs_tab(&mut self, down: bool, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let s = self.store.read(cx);
        let friends = s.mailbox_friends().to_vec();
        let insts: Vec<Instance> = s.instances().iter().filter(|i| !i.id.is_empty()).cloned().collect();
        if insts.is_empty() {
            return widgets::empty_state(IconName::Package, "No packs yet", Some("Install a pack, and it shows up here to share with your friends.".into()), cx)
                .into_any_element();
        }
        let rows: Vec<AnyElement> = insts
            .iter()
            .enumerate()
            .map(|(i, inst)| {
                let s = self.store.read(cx);
                // Friends first, then groups.
                let who: Vec<String> = friends
                    .iter()
                    .filter(|f| s.settings().shares_with(&inst.id, &f.id))
                    .map(|f| name_or(&f.name, "a friend"))
                    .chain(
                        s.mailbox_groups()
                            .iter()
                            .filter(|g| g.me.joined && s.settings().shares_with_group(&inst.id, &g.id))
                            .map(|g| g.name.clone()),
                    )
                    .collect();
                let sub = match who.len() {
                    0 => "Not shared".to_string(),
                    1..=3 => who.join(", "),
                    n => format!("{}, {} and {} more", who[0], who[1], n - 2),
                };
                let icon = inst.icon.as_deref().and_then(|u| s.image(u)).map(Path::to_path_buf);
                let open = self.share_pop.as_deref() == Some(inst.id.as_str());
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
                                widgets::caption(sub, cx)
                                    .text_xs()
                                    .pl(rems(1.625))
                                    .when(!who.is_empty(), |d| d.text_color(theme.foreground)),
                            ),
                    )
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
            .child(div().px_3().pt_3().pb_2().child(widgets::fine_print(
                "Every pack starts private. Share one, and those friends get it plus every change you make. Only the mod list is sent. The mods download from their own sites.",
                cx,
            )))
            .children(rows)
            .into_any_element()
    }
}

impl Render for FriendsPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let s = self.store.read(cx);
        let status = s.sync_status();
        let heard = !s.mailbox_friends().is_empty() || !s.mailbox_recipes().is_empty();
        let n_friends = s.mailbox_friends().len();
        let n_shared = s
            .instances()
            .iter()
            .filter(|i| !i.id.is_empty() && s.settings().shared_with.iter().any(|x| x.inst == i.id))
            .count();
        let down = matches!(status, SyncStatus::Down(_));
        let body: AnyElement = match &status {
            SyncStatus::Off => self.off(cx),
            SyncStatus::Connecting if !heard => {
                v_flex().p_3().child(widgets::skeleton_rows(5, rems(2.75), cx)).into_any_element()
            }
            _ => {
                // Down before the relay ever answered: the friend list isn't
                // known, so don't show the "no friends yet" pitch.
                let (top, content) = if self.tab == FRIENDS && down && !heard {
                    (None, div().into_any_element())
                } else if self.tab == FRIENDS {
                    self.friends_tab(down, window, cx)
                } else {
                    (None, self.packs_tab(down, cx))
                };
                // Each page keeps its own scroll position, and a fresh one
                // starts at the top.
                let scroll_id = match (&self.page, self.tab) {
                    (Page::Friend(id), FRIENDS) => SharedString::from(format!("friends-scroll:f:{id}")),
                    (Page::Group(id), FRIENDS) => SharedString::from(format!("friends-scroll:g:{id}")),
                    (Page::Ended, FRIENDS) => SharedString::from("friends-scroll:ended"),
                    _ => SharedString::from("friends-scroll"),
                };
                v_flex()
                    .size_full()
                    // No count while the list isn't known yet.
                    .child(self.tabs((heard || !down).then_some(n_friends), n_shared, cx))
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
                    .child(div().id(scroll_id).flex_1().min_h_0().overflow_y_scrollbar().child(content))
                    .into_any_element()
            }
        };
        let content = v_flex()
            .size_full()
            .track_focus(&self.focus)
            // Escape on a page goes back to the list.
            .on_key_down(cx.listener(|this, e: &KeyDownEvent, window, cx| {
                if e.keystroke.key == "escape" && this.page != Page::List && !this.plus_pop {
                    this.open(Page::List, window, cx);
                    cx.stop_propagation();
                }
            }))
            .child(self.header(cx))
            .child(div().flex_1().min_h_0().child(body));
        // The same dock as the mod peek: its width, edge, border and look.
        // Its id follows the page, so a page slides in like the panel does.
        let dock_id = match &self.page {
            Page::List => SharedString::from("friends-dock"),
            Page::Friend(id) => SharedString::from(format!("friends-dock:f:{id}")),
            Page::Group(id) => SharedString::from(format!("friends-dock:g:{id}")),
            Page::Ended => SharedString::from("friends-dock:ended"),
        };
        widgets::dock(dock_id, &self.panel, |t: &mut Self| &mut t.panel, &self.store, content, window, cx)
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
fn face(store: &Entity<Store>, mc_id: &str, name: &str, size: Rems, cx: &mut App) -> AnyElement {
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
        assert!(strings.iter().any(|s| s == "Invite a friend, join with a code, or make a group"));
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
