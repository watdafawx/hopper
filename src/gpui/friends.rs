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
use hopper::mailbox::{Event, Friend};
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

/// The tabs.
const FRIENDS: usize = 0;
const YOUR_PACKS: usize = 1;

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
    _subs: Vec<Subscription>,
}

impl FriendsPanel {
    pub fn new(store: Entity<Store>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let join_code = cx.new(|cx| InputState::new(window, cx).placeholder("Paste their code"));
        let subs = vec![
            cx.observe(&store, |_, _, cx| cx.notify()),
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
            _subs: subs,
        }
    }

    /// The panel's width in rem, for the page beside it.
    pub fn width(&self, window: &Window, cx: &App) -> f32 {
        self.panel.layout(self.store.read(cx).settings(), window).width
    }

    /// Opens the share popover of a pack (instance id), or closes it.
    fn set_pop(&mut self, pop: Option<String>, cx: &mut Context<Self>) {
        if self.share_pop != pop {
            self.share_pop = pop;
            cx.emit(FriendsEvent::Popover);
        }
        cx.notify();
    }

    /// The panel was opened or closed.
    pub fn shown(&mut self, open: bool, cx: &mut Context<Self>) {
        self.set_pop(None, cx);
        if open {
            self.store.update(cx, |s, cx| s.mark_friends_seen(cx));
        }
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
                        let live = this.store.read(cx).mailbox_invite().is_some();
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
                                "{name} stops getting changes to your packs, and you stop getting theirs. Instances you follow from {name} stay as they are. You can invite {name} again later."
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
                    .child(format!("Who sees {}", inst.name)),
            )
            .when(friends.is_empty(), |c| {
                c.child(div().px_2().child(widgets::fine_print("Invite a friend first; then pick who sees this pack.", cx)))
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
                    .children(rows),
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
                    .child(widgets::fine_print(
                        if takes_back {
                            "Turning someone off stops their updates and takes the pack off their list. Their copy stays as it is."
                        } else {
                            "Turning someone off stops new changes. They keep what they already have."
                        },
                        cx,
                    ))
                    .child(
                        h_flex().child(
                            Button::new("share-pop-code")
                                .small()
                                .ghost()
                                .icon(IconName::Copy)
                                .label("Copy a share code")
                                .on_click(copy),
                        ),
                    ),
            );
        Some(place.apply(pop, 17., 22., window).into_any_element())
    }

    /// A friend's face, from their Minecraft account; a plain circle when
    /// they aren't signed in.
    fn head(&self, mc_id: &str, name: &str, size: Rems, cx: &mut Context<Self>) -> AnyElement {
        if mc_id.is_empty() {
            let theme = cx.theme();
            return div()
                .flex_none()
                .size(size)
                .rounded_full()
                .bg(theme.muted)
                .flex()
                .items_center()
                .justify_center()
                .child(Icon::new(IconName::User).xsmall().text_color(theme.muted_foreground))
                .into_any_element();
        }
        let saved = hopper::accounts::Saved { id: mc_id.to_string(), name: name.to_string(), ..Default::default() };
        let url = saved.fallback_avatar();
        self.store.update(cx, |s, _| s.want_image_now(&url));
        div().flex_none().child(crate::accounts::head(self.store.read(cx), &saved, size, cx)).into_any_element()
    }

    fn header(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        h_flex()
            .flex_none()
            .h(rems(3.))
            .px_3()
            .gap_2()
            .child(div().flex_1().text_base().font_semibold().child("Friends"))
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

    /// Friends & sync is off: what it is, and a button to turn it on.
    fn off(&self, cx: &mut Context<Self>) -> AnyElement {
        let err = self.store.read(cx).peer_error();
        v_flex()
            .size_full()
            .justify_center()
            .child(
                widgets::empty_state(
                    IconName::Users,
                    "Friends is off",
                    Some(
                        "Turn it on to see your friends' packs and share yours. Hopper sends mod lists through its relay; no account needed."
                            .into(),
                    ),
                    cx,
                )
                .child(
                    Button::new("friends-on")
                        .primary()
                        .label("Turn on")
                        .on_click(cx.listener(|this, _, _, cx| this.store.update(cx, |s, cx| s.set_peer_sync(true, cx)))),
                )
                .when_some(err, |c, e| c.child(div().text_sm().text_color(cx.theme().danger).child(e))),
            )
            .into_any_element()
    }

    /// No friends and nothing shared with this PC yet: what Friends does,
    /// with the invite and join right there.
    fn empty(&mut self, down: bool, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        v_flex()
            .w_full()
            .child(
                widgets::empty_state(
                    IconName::Users,
                    "Play the same packs as your friends",
                    Some(
                        "Follow a friend's pack and yours changes when theirs does. Only the mod list is sent; the files download from CurseForge, Modrinth and GitHub."
                            .into(),
                    ),
                    cx,
                )
                .pb_4(),
            )
            .child(v_flex().px_4().gap_3().child(self.invite_block(down, cx)).child(
                h_flex()
                    .gap_2()
                    .child(div().flex_1().h_px().bg(theme.border))
                    .child(widgets::caption("or", cx).text_xs())
                    .child(div().flex_1().h_px().bg(theme.border)),
            ).child(self.join_block(down, cx)).child(widgets::fine_print(
                "No account needed. Codes and mod lists go through Hopper's relay; nothing else leaves this PC.",
                cx,
            )))
            .into_any_element()
    }

    /// The live invite's code and countdown, or the Invite button.
    fn invite_block(&mut self, down: bool, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let s = self.store.read(cx);
        if let Some(inv) = s.mailbox_invite() {
            self.tick(cx);
            let left = inv.until.saturating_duration_since(Instant::now()).as_secs();
            let shown = if inv.code.len() == 8 { format!("{} {}", &inv.code[..4], &inv.code[4..]) } else { inv.code.clone() };
            let copy = inv.code.clone();
            let frac = (left as f32 / INVITE_SECS).clamp(0., 1.);
            return v_flex()
                .gap_2()
                .p_3()
                .rounded(theme.radius_lg)
                .border_1()
                .border_color(theme.border)
                .child(div().text_sm().font_medium().child("Send this code to your friend"))
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
                                .on_click(move |_, window, cx| {
                                    cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()));
                                    window.push_notification("Code copied.", cx);
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
                    format!("Waiting for them to join · Works once · {:02}:{:02} left", left / 60, left % 60),
                    cx,
                ))
                .into_any_element();
        }
        if s.friends_full() {
            return v_flex()
                .gap_1()
                .child(Button::new("invite").w_full().primary().label("Friend list full").disabled(true))
                .child(widgets::fine_print("Remove a friend to add another.", cx))
                .into_any_element();
        }
        Button::new("invite")
            .w_full()
            .primary()
            .icon(IconName::UserPlus)
            .label("Invite a friend")
            .disabled(down)
            .when(down, |b| b.tooltip("Hopper's relay can't be reached right now"))
            .on_click(cx.listener(|this, _, _, cx| this.store.update(cx, |s, cx| s.make_invite("", cx))))
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
            .child(div().text_sm().font_semibold().child("Your friends can't see your packs yet"))
            // Muted grey drops under 4.5:1 on the orange tint in light themes.
            .child(div().text_xs().text_color(theme.foreground.opacity(0.8)).child("Every pack starts private. Pick which ones each friend sees."))
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

    /// One friend: their row, then the packs they share with this PC.
    fn friend_section(
        &self,
        ix: usize,
        f: Option<&Friend>,
        id: &str,
        name: &str,
        theirs: Vec<Event>,
        down: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let s = self.store.read(cx);
        let sees = s
            .instances()
            .iter()
            .filter(|i| !i.id.is_empty() && s.settings().shares_with(&i.id, id))
            .count();
        // With the relay down, "online" is stale: show when they were last
        // seen instead.
        let status = match f {
            Some(f) if f.online && !down => "Online".to_string(),
            Some(f) if down && f.last_seen == 0 => "Online status unknown".to_string(),
            Some(f) if f.last_seen > 0 => format!("Last online {}", ago(f.last_seen)),
            Some(_) => "Offline".to_string(),
            None => "Shares through a group".to_string(),
        };
        let sub = match (f, sees) {
            (None, _) => status,
            (_, 0) => format!("{status} · sees none of your packs"),
            (_, n) => format!("{status} · sees {n} of your packs"),
        };
        let mc_id = f.map(|f| f.mc_id.clone()).unwrap_or_default();
        let head = self.head(&mc_id, name, rems(1.75), cx);
        let menu = f.cloned().map(|f| {
            let view = cx.entity();
            let first = name.to_string();
            Button::new(("friend-menu", ix))
                .ghost()
                .xsmall()
                .icon(IconName::Ellipsis)
                .tooltip("More")
                .dropdown_menu_with_anchor(Anchor::TopRight, move |menu, _, _| {
                    let (v1, v2, f2) = (view.clone(), view.clone(), f.clone());
                    menu.item(PopupMenuItem::new(format!("Choose packs for {first}")).icon(IconName::Share2).on_click(move |_, _, cx| {
                        v1.update(cx, |t, cx| {
                            t.tab = YOUR_PACKS;
                            cx.notify();
                        })
                    }))
                    .separator()
                    .item(
                        PopupMenuItem::new("Remove friend…")
                            .icon(IconName::UserMinus)
                            .on_click(move |_, window, cx| v2.update(cx, |t, cx| t.confirm_remove(f2.clone(), window, cx))),
                    )
                })
        });
        let online = !down && f.is_some_and(|f| f.online);
        let pack_rows: Vec<AnyElement> =
            theirs.iter().enumerate().map(|(pi, e)| self.their_pack(ix, pi, e, down, cx)).collect();
        let none = theirs.is_empty();
        v_flex()
            .py_1()
            .child(
                h_flex()
                    .id(("friend", ix))
                    .gap_2()
                    .px_3()
                    .py_1p5()
                    .hover(|s| widgets::row_hover(s, cx))
                    .child(div().relative().child(head).when(online, |d| {
                        d.child(
                            div()
                                .absolute()
                                .right(px(-1.))
                                .bottom(px(-1.))
                                .size(px(9.))
                                .rounded_full()
                                .border_2()
                                .border_color(theme.background)
                                .bg(theme.success),
                        )
                    }))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(div().text_sm().font_medium().truncate().child(name.to_string()))
                            .child(widgets::caption(sub, cx).text_xs()),
                    )
                    .children(menu),
            )
            .children(pack_rows)
            .when(none && f.is_some(), |c| {
                c.child(div().pl(rems(3.25)).pr_3().pb_1().child(widgets::fine_print(format!("{name} hasn't shared a pack yet."), cx)))
            })
            .into_any_element()
    }

    /// A pack a friend shares: Following (into which instance) or Follow.
    fn their_pack(&self, fi: usize, pi: usize, e: &Event, down: bool, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let s = self.store.read(cx);
        let follows: Vec<(PathBuf, String, bool)> = s
            .followers_of(&e.from, &e.inst)
            .into_iter()
            .map(|f| {
                let root = PathBuf::from(&f.root);
                let local = s.instance(&root).map(|i| i.name.clone()).unwrap_or_else(|| f.name.clone());
                (root, local, !f.ended.is_empty())
            })
            .collect();
        let (icon, project) = pack_look(s, &e.code);
        let pack = if e.name.is_empty() { e.inst.clone() } else { e.name.clone() };
        let updated = if e.at > 0 { format!("updated {}", ago(e.at)) } else { String::new() };
        let sub = match follows.first() {
            Some((_, local, _)) if updated.is_empty() => format!("Into {local}"),
            Some((_, local, _)) => format!("Into {local} · {updated}"),
            None if updated.is_empty() => String::new(),
            None => capitalise(&updated),
        };
        let stale = follows.is_empty() && e.at > 0 && now().saturating_sub(e.at) > STALE_SECS;
        let id = SharedString::from(format!("their:{fi}:{pi}"));
        let view = cx.entity();
        let name_el = widgets::mod_link(
            id.clone(),
            pack,
            icon.as_deref(),
            project,
            move |p, _, cx| view.update(cx, |_, cx| cx.emit(FriendsEvent::Project(p))),
            cx,
        )
        .text_sm();
        let action: AnyElement = match follows.first() {
            Some(_) => widgets::chip(theme.success).child("Following").into_any_element(),
            None => {
                let e = e.clone();
                Button::new(SharedString::from(format!("follow:{fi}:{pi}")))
                    .xsmall()
                    .outline()
                    .label("Follow")
                    .disabled(down)
                    .when(down, |b| b.tooltip("Hopper's relay can't be reached right now"))
                    .on_click(cx.listener(move |this, _, window, cx| this.follow(e.clone(), window, cx)))
                    .into_any_element()
            }
        };
        let hide = stale.then(|| {
            let e = e.clone();
            Button::new(SharedString::from(format!("hide:{fi}:{pi}")))
                .xsmall()
                .ghost()
                .icon(IconName::Close)
                .tooltip("Hide until it changes")
                .on_click(cx.listener(move |this, _, _, cx| this.store.update(cx, |s, cx| s.hide_friend_pack(&e, cx))))
        });
        h_flex()
            .id(id)
            .gap_2()
            .pl(rems(3.25))
            .pr_3()
            .py_1()
            .hover(|s| widgets::row_hover(s, cx))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(name_el)
                    .when(!sub.is_empty(), |c| c.child(widgets::caption(sub, cx).text_xs().pl(rems(1.625))))
                    .when(!e.left_out.is_empty(), |c| {
                        c.child(
                            widgets::caption(
                                format!(
                                    "{} file{} can't be downloaded from any site",
                                    e.left_out.len(),
                                    if e.left_out.len() == 1 { "" } else { "s" }
                                ),
                                cx,
                            )
                            .text_xs()
                            .pl(rems(1.625)),
                        )
                    }),
            )
            .child(action)
            .children(hide)
            .into_any_element()
    }

    /// Follows that get nothing more: the friend removed this PC, or the
    /// pack is gone. Each can be stopped; the instance stays.
    fn ended(&self, friends: &[Friend], recipes: &[Event], cx: &mut Context<Self>) -> Option<AnyElement> {
        let s = self.store.read(cx);
        let ended: Vec<(PathBuf, String, String, String)> = s
            .settings()
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
                    format!("{} · no longer your friend", name_or(&f.peer_name, "A friend"))
                };
                (root, f.name.clone(), local, who)
            })
            .collect();
        if ended.is_empty() {
            return None;
        }
        let theme = cx.theme().clone();
        Some(
            v_flex()
                .py_1()
                .border_t_1()
                .border_color(theme.border)
                .child(div().px_3().pt_2().pb_1().child(widgets::caption("No more updates", cx).text_xs().font_medium()))
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
                                .child(widgets::caption(format!("{who} · into {local}"), cx).text_xs()),
                        )
                        .child(
                            Button::new(("stop-follow", i))
                                .xsmall()
                                .outline()
                                .label("Stop following")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    let root = root.clone();
                                    this.store.update(cx, |s, cx| s.stop_following(&root, cx))
                                })),
                        )
                }))
                .child(div().px_3().pb_1().child(widgets::fine_print(
                    "Your instance stays as it is. Stop following to drop it from this list.",
                    cx,
                )))
                .into_any_element(),
        )
    }

    fn friends_tab(&mut self, down: bool, cx: &mut Context<Self>) -> AnyElement {
        let s = self.store.read(cx);
        let friends = s.mailbox_friends().to_vec();
        let recipes: Vec<Event> = s.mailbox_recipes().iter().filter(|e| !s.pack_hidden(e)).cloned().collect();
        let all_recipes = s.mailbox_recipes().to_vec();
        let prompt = !friends.is_empty() && s.settings().shared_with.is_empty() && !s.settings().share_prompt_done;
        if friends.is_empty() && all_recipes.is_empty() {
            let ended = self.ended(&friends, &all_recipes, cx);
            return v_flex().children(ended).child(self.empty(down, cx)).into_any_element();
        }
        // One section per friend; someone who shares through a group but
        // isn't a friend gets one too, so their pack doesn't vanish.
        let mut people: Vec<(Option<Friend>, String, String)> =
            friends.iter().map(|f| (Some(f.clone()), f.id.clone(), name_or(&f.name, "A friend"))).collect();
        for e in &recipes {
            if !people.iter().any(|p| p.1 == e.from) {
                people.push((None, e.from.clone(), name_or(&e.from_name, "A friend")));
            }
        }
        let sections: Vec<AnyElement> = people
            .iter()
            .enumerate()
            .map(|(i, (f, id, name))| {
                let theirs: Vec<Event> = recipes.iter().filter(|e| &e.from == id).cloned().collect();
                self.friend_section(i, f.as_ref(), id, name, theirs, down, cx)
            })
            .collect();
        let ended = self.ended(&friends, &all_recipes, cx);
        v_flex()
            .when(prompt, |c| c.child(div().p_3().child(self.prompt(cx))))
            .children(sections)
            .children(ended)
            .into_any_element()
    }

    fn packs_tab(&mut self, down: bool, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let s = self.store.read(cx);
        let friends = s.mailbox_friends().to_vec();
        let insts: Vec<Instance> = s.instances().iter().filter(|i| !i.id.is_empty()).cloned().collect();
        if insts.is_empty() {
            return widgets::empty_state(IconName::Package, "No packs yet", Some("Packs you install show here, to share with friends.".into()), cx)
                .into_any_element();
        }
        let rows: Vec<AnyElement> = insts
            .iter()
            .enumerate()
            .map(|(i, inst)| {
                let s = self.store.read(cx);
                let who: Vec<&Friend> = friends.iter().filter(|f| s.settings().shares_with(&inst.id, &f.id)).collect();
                let sub = match who.len() {
                    0 => "Only you".to_string(),
                    1..=3 => who.iter().map(|f| name_or(&f.name, "a friend")).collect::<Vec<_>>().join(", "),
                    n => format!("{}, {} and {} more", name_or(&who[0].name, "a friend"), name_or(&who[1].name, "a friend"), n - 2),
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
                                    .hover(|s| s.text_color(theme.primary))
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
                                    .when(down, |b| b.tooltip("Hopper's relay can't be reached right now"))
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
                "Pick who sees each pack. They get its mod list and every change you make; files download from the mods' own sites.",
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
                let content = if self.tab == FRIENDS && down && !heard {
                    div().into_any_element()
                } else if self.tab == FRIENDS {
                    self.friends_tab(down, cx)
                } else {
                    self.packs_tab(down, cx)
                };
                let has_friends = n_friends > 0 || heard;
                let footer = (self.tab == FRIENDS && has_friends).then(|| {
                    v_flex()
                        .flex_none()
                        .gap_3()
                        .p_3()
                        .border_t_1()
                        .border_color(theme.border)
                        .child(self.invite_block(down, cx))
                        .child(self.join_block(down, cx))
                });
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
                                        .child(div().text_sm().font_medium().child("Can't reach Hopper's relay"))
                                        .child(widgets::fine_print(
                                            // The plain "can't reach" message would repeat the title.
                                            match (heard, e.starts_with("Can't reach")) {
                                                (false, _) => "Your friends show here once Hopper reaches it. It keeps trying.".to_string(),
                                                (true, true) => "Showing what you had. Hopper keeps trying.".to_string(),
                                                (true, false) => format!("Showing what you had. {e}"),
                                            },
                                            cx,
                                        )),
                                ),
                            ),
                        )
                    })
                    .child(div().id("friends-scroll").flex_1().min_h_0().overflow_y_scrollbar().child(content))
                    .children(footer)
                    .into_any_element()
            }
        };
        let content = v_flex().size_full().child(self.header(cx)).child(div().flex_1().min_h_0().child(body));
        // The same dock as the mod peek: its width, edge, border and look.
        widgets::dock("friends-dock", &self.panel, |t: &mut Self| &mut t.panel, &self.store, content, window, cx)
    }
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
                (format!("{who}'s pack is newer: yours updates first, then the mods"), (theme.primary, "Pack update"))
            } else {
                let what = if self.plain { "Same Minecraft and loader" } else { "Same pack" };
                (what.to_string(), (theme.success, "Fits"))
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
            "Installs their pack as a new instance. Nothing here changes.".into(),
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
                "When {who} changes this pack, yours changes with it. Only the mod list comes from {who}; files download from the mods' own sites."
            )))
            .child(widgets::caption("Put it in", cx).text_xs().font_medium())
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
                            "Your mods and settings in {inst} are replaced with {who}'s, now and each time {who} changes the pack. History can undo each change."
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

fn capitalise(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().chain(c).collect(),
        None => String::new(),
    }
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
}
