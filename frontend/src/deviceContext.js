const KEY = 'youn_selected_device';

export function getSelectedDevice() {
  return localStorage.getItem(KEY) || '';
}

export function setSelectedDevice(id) {
  if (id) localStorage.setItem(KEY, id);
  else localStorage.removeItem(KEY);
  window.dispatchEvent(new Event('device-changed'));
}
