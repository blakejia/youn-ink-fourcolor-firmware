import { getToken } from './auth.js';

const BASE = '/api';

async function request(path, opts = {}) {
  const headers = { ...(opts.headers || {}) };
  const token = getToken();
  if (token) headers['X-Operator-Token'] = token;
  if (opts.body && typeof opts.body !== 'string') {
    headers['Content-Type'] = 'application/json';
    opts.body = JSON.stringify(opts.body);
  }
  const res = await fetch(BASE + path, { ...opts, headers });
  if (!res.ok) {
    let detail = res.statusText;
    try {
      const j = await res.json();
      if (j.detail) detail = typeof j.detail === 'string' ? j.detail : JSON.stringify(j.detail);
    } catch (e) { /* not json */ }
    throw new Error(detail);
  }
  const ct = res.headers.get('content-type') || '';
  if (ct.includes('application/json')) return res.json();
  return res;
}

export const apiFetch = request;
export const api = {
  health: () => request('/health'),
  devices: async () => (await request('/devices')).devices ?? [],
  approve: (id) => request(`/devices/${id}/approve`, { method: 'POST' }),
  revoke: (id) => request(`/devices/${id}/revoke`, { method: 'POST' }),
  pairConfirm: (deviceId, code) =>
    request('/devices/pair-confirm', { method: 'POST', body: { device_id: deviceId, code } }),
  pairPending: async () => (await request('/devices/pair-pending')).sessions ?? [],
  pages: async (device) => (await request(`/pages?device=${encodeURIComponent(device)}`)).pages ?? [],
  createPage: (data) => request('/pages', { method: 'POST', body: data }),
  deletePage: (name, device) =>
    request(`/pages/${encodeURIComponent(name)}?device=${encodeURIComponent(device)}`, { method: 'DELETE' }),
  preview: async (canvasJson) => {
    const res = await request('/pages/preview', { method: 'POST', body: { canvas_json: canvasJson } });
    return res.blob();
  },
  uploadToPage: async (file, page, device) => {
    const fd = new FormData();
    fd.append('image', file);
    fd.append('page', page);
    fd.append('device', device);
    const headers = {};
    const token = getToken();
    if (token) headers['X-Operator-Token'] = token;
    const res = await fetch(`${BASE}/uploads`, { method: 'POST', body: fd, headers });
    if (!res.ok) {
      let d = res.statusText;
      try {
        const body = await res.json();
        d = (body.detail && body.detail.detail) || body.detail || d;
        if (body.detail && body.detail.pages) d += `（可选页面：${body.detail.pages.join('、')}）`;
      } catch (e) {}
      throw new Error(d);
    }
    return res.json();
  },
  otaCheck: async () => {
    // operator view of latest firmware (device-facing /ota/check needs Bearer)
    const j = await request('/ota');
    return j;
  },
  uploadOta: async (file, version, channel, notes) => {
    const fd = new FormData();
    fd.append('firmware', file);
    fd.append('version', version);
    if (channel) fd.append('channel', channel);
    if (notes) fd.append('notes', notes);
    const headers = {};
    const token = getToken();
    if (token) headers['X-Operator-Token'] = token;
    const res = await fetch(`${BASE}/ota`, { method: 'POST', body: fd, headers });
    if (!res.ok) {
      let d = res.statusText;
      try { d = (await res.json()).detail || d; } catch (e) {}
      throw new Error(d);
    }
    return res.json();
  },
};
