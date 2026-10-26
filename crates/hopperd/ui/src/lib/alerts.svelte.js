// Alerts (crash, low TPS, drive nearly full, PC nearly out of memory) and the
// latest TPS of each server, polled while any part of the page shows them.
import { api, post } from './api.js';

export const alerts = $state({ active: [], tps: {}, no_tps: {}, settings: null });

let timer = null;
let users = 0;

export async function refreshAlerts() {
  try {
    Object.assign(alerts, await api('/alerts'));
  } catch {
    // signed out or offline: keep the last look
  }
}

/** Starts polling; call the returned function to stop. */
export function watchAlerts() {
  users++;
  if (users === 1) {
    refreshAlerts();
    timer = setInterval(refreshAlerts, 15000);
  }
  return () => {
    users--;
    if (!users) {
      clearInterval(timer);
      timer = null;
    }
  };
}

export async function dismissAlert(a) {
  try {
    await post('/alerts/dismiss', { server_id: a.server_id, kind: a.kind });
  } finally {
    refreshAlerts();
  }
}

/** Alerts about one server, plus the panel-wide ones (drives, memory). */
export const alertsFor = (id) => alerts.active.filter((a) => a.server_id === id || !a.server_id);
