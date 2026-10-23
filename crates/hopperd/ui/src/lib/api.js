// Calls to hopperd's API. Every request carries X-Hopperd, which the panel
// requires from browsers (other sites can't add it).

export class ApiError extends Error {
  constructor(status, message) {
    super(message);
    this.status = status;
  }
}

export async function api(path, { method = 'GET', body } = {}) {
  const res = await fetch(`/api/v1${path}`, {
    method,
    headers: { 'X-Hopperd': '1', ...(body ? { 'Content-Type': 'application/json' } : {}) },
    body: body ? JSON.stringify(body) : undefined,
    credentials: 'same-origin',
  });
  let data = null;
  try {
    data = await res.json();
  } catch {
    // empty body
  }
  if (!res.ok) throw new ApiError(res.status, data?.error || `${res.status} ${res.statusText}`);
  return data;
}

export const post = (path, body = {}) => api(path, { method: 'POST', body });
export const del = (path) => api(path, { method: 'DELETE' });
