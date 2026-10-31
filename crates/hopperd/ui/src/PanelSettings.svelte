<script module>
  import { FolderCog, Shield, Globe, Compass, Lock, Users as UsersIcon, Webhook, BellRing, CloudUpload, UserRound, Download, Zap } from '@lucide/svelte';

  // The panel's settings, by category, like Hopper desktop's Settings page
  // (src/gpui/settings_page.rs): a list of categories on the left, the
  // picked one's icon, title and line under it, then its cards.
  export const CATS = [
    { id: 'panel-folder', label: 'Panel folder', icon: FolderCog, owner: true, blurb: 'Where the servers and the panel’s own data live, servers deleted lately, and how long backups stay.', words: 'servers folder move data deleted restore backups undo hopper-backups clean up retention disk space' },
    { id: 'access', label: 'Access & devices', icon: Shield, owner: true, blurb: 'Pair Hopper desktop, how clients reach this panel, and the devices allowed in.', words: 'pair pairing devices tls certificate sftp https addresses' },
    { id: 'addresses', label: 'Server addresses', icon: Globe, owner: true, blurb: 'Players join by name on one shared Minecraft port.', words: 'domain port router proxy join' },
    { id: 'content', label: 'CurseForge & Modrinth', icon: Compass, owner: true, blurb: 'The CurseForge API key.', words: 'curseforge api key modrinth' },
    { id: 'security', label: 'Security', icon: Lock, owner: true, blurb: 'Two-factor sign-in, your password, API tokens and the audit log.', words: '2fa two-factor password tokens audit log' },
    { id: 'users', label: 'Users', icon: UsersIcon, owner: true, blurb: 'Others sign in with their own name and password, limited to their role and servers.', words: 'people accounts roles permissions' },
    { id: 'webhooks', label: 'Webhooks', icon: Webhook, owner: true, blurb: 'Tell Discord or another app when servers start, stop, crash, back up or run low.', words: 'discord notify hooks' },
    { id: 'alerts', label: 'Alerts', icon: BellRing, owner: true, blurb: 'When the panel warns you: crashes, low TPS, a nearly full drive, PC memory.', words: 'warnings crash disk memory notify' },
    { id: 'automations', label: 'Automations', icon: Zap, owner: true, blurb: 'When a server does something, the panel acts: run a command, back up, restart, or call a URL.', words: 'rules automation trigger when then if restart lag tps players schedule timer cron command' },
    { id: 'backups', label: 'Backup copies', icon: CloudUpload, owner: true, blurb: 'Copy backups off this PC: a NAS or USB disk, SFTP, S3 / B2 / R2, or another Hopper Server.', words: 'offsite off-site s3 copies backups' },
    { id: 'updates', label: 'Updates', icon: Download, owner: true, blurb: 'Hopper Server’s version, and updating it by itself.', words: 'update version auto-update upgrade release' },
    { id: 'account', label: 'Account', icon: UserRound, owner: false, blurb: 'Your sign-in: two-factor and your password.', words: 'password 2fa me' },
  ];

  /** The categories this person sees. */
  export const catsFor = (owner) => CATS.filter((c) => c.owner === owner);
</script>

<script>
  import Access from './Access.svelte';
  import ServersFolder from './ServersFolder.svelte';
  import PanelUpdate from './PanelUpdate.svelte';
  import Addresses from './Addresses.svelte';
  import CfKey from './CfKey.svelte';
  import Security from './Security.svelte';
  import Users from './Users.svelte';
  import { ownerLike } from './lib/me.svelte.js';
  import { rightClear } from './lib/rightclear.js';
  import Webhooks from './Webhooks.svelte';
  import Alerts from './Alerts.svelte';
  import Automations from './Automations.svelte';
  import BackupTargets from './BackupTargets.svelte';
  import BackupRetention from './BackupRetention.svelte';

  let { cat = '', hello, me, pending = $bindable([]) } = $props();
  const owner = $derived(ownerLike(me));
  const cats = $derived(catsFor(owner));
  const c = $derived(cats.find((x) => x.id === cat) || cats[0]);
  let query = $state('');
  const hits = $derived.by(() => {
    const q = query.trim().toLowerCase();
    if (!q) return null;
    return cats.filter((x) => `${x.label} ${x.blurb} ${x.words}`.toLowerCase().includes(q));
  });
</script>

<div class="page">
  <nav class="cats">
    <div class="input sm"><input use:rightClear bind:value={query} placeholder="Search settings" spellcheck="false" onkeydown={(e) => { if (e.key === 'Enter' && hits?.length) { location.hash = `/settings/${hits[0].id}`; query = ''; } }} /></div>
    {#if hits}
      <div class="lbl hits">{hits.length === 0 ? 'No matches' : hits.length === 1 ? '1 match' : `${hits.length} matches`}</div>
      {#each hits as x, i (x.id)}
        <a class="hit row-hover" class:first={i === 0} href="#/settings/{x.id}" onclick={() => (query = '')}>
          <span>{x.label}</span><span class="tiny muted">{x.blurb}</span>
        </a>
      {/each}
    {:else}
      {#each cats as x (x.id)}
        {@const Icon = x.icon}
        <a class="cat" class:row-sel={x.id === c.id} class:row-hover={x.id !== c.id} href="#/settings/{x.id}">
          <Icon size={15} color={x.id === c.id ? 'var(--acc)' : 'var(--muted)'} /><span>{x.label}</span>
          {#if x.id === 'access' && pending.length}<span class="badge">{pending.length}</span>{/if}
        </a>
      {/each}
    {/if}
  </nav>
  <div class="pane">
    <div class="inner">
      <header class="head">
        <span class="tile">{#if c.icon}{@const Icon = c.icon}<Icon size={18} />{/if}</span>
        <div><h1>{c.label}</h1><p>{c.blurb}</p></div>
      </header>
      <div class="body">
        {#if c.id === 'panel-folder'}<ServersFolder /><BackupRetention />
        {:else if c.id === 'access' || c.id === 'account'}<Access {hello} {me} bind:pending />
        {:else if c.id === 'addresses'}<Addresses />
        {:else if c.id === 'content'}<CfKey />
        {:else if c.id === 'security'}<Security />
        {:else if c.id === 'users'}<Users />
        {:else if c.id === 'webhooks'}<Webhooks />
        {:else if c.id === 'alerts'}<Alerts />
        {:else if c.id === 'automations'}<Automations />
        {:else if c.id === 'backups'}<BackupTargets />
        {:else if c.id === 'updates'}<PanelUpdate />
        {/if}
      </div>
    </div>
  </div>
</div>

<style>
  .page { flex: 1; min-height: 0; display: flex; }
  .cats { width: 232px; flex: none; display: flex; flex-direction: column; gap: 2px; padding: 16px 10px; border-right: 1px solid var(--line); overflow-y: auto; }
  .cats .input { margin-bottom: 10px; padding: 5px 9px; }
  .cat { display: flex; align-items: center; gap: 10px; height: 32px; padding: 0 10px; border-radius: var(--radius); color: var(--text); }
  .cat.row-sel { font-weight: 500; }
  .badge { margin-left: auto; background: var(--acc); color: #1c0d02; border-radius: 99px; font-size: 11px; font-weight: 700; padding: 0 6px; }
  .hits { padding: 0 10px 4px; }
  .hit { display: flex; flex-direction: column; padding: 6px 10px; border-radius: var(--radius); color: var(--text); }
  .hit.first { background: var(--hover); }
  .pane { flex: 1; min-width: 0; overflow-y: auto; padding: 24px 32px 48px; }
  .inner { max-width: 768px; margin: 0 auto; display: flex; flex-direction: column; gap: 16px; }
  .head { display: flex; align-items: center; gap: 16px; padding-bottom: 8px; }
  .tile { width: 44px; height: 44px; flex: none; display: grid; place-items: center; border-radius: var(--radius-lg); background: #f973161f; border: 1px solid #f9731640; color: var(--acc); }
  .head h1 { font-size: 22px; font-weight: 600; line-height: 1.25; }
  .head p { color: var(--muted); font-size: 12.5px; }
  .body { display: flex; flex-direction: column; gap: 14px; }
  .body :global(.wide) { grid-column: auto; }
  @media (max-width: 760px) {
    .page { flex-direction: column; }
    .cats { width: 100%; flex-direction: row; flex-wrap: wrap; border-right: 0; border-bottom: 1px solid var(--line); padding: 8px 16px; }
    .cats .input, .hits { width: 100%; }
    .cat { height: 30px; }
    .pane { padding: 16px; }
  }
</style>
