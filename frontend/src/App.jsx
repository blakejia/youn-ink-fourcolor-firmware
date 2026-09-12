import React, { useEffect, useState } from 'react';
import { Navigate, NavLink, Outlet, useNavigate } from 'react-router-dom';
import { isAuthed, clearToken } from './auth.js';
import { api } from './api.js';
import { getSelectedDevice, setSelectedDevice } from './deviceContext.js';
import { hasUnsaved } from './unsaved.js';
import { ConfirmDialog } from './ui.jsx';
import Login from './pages/Login.jsx';
import Devices from './pages/Devices.jsx';
import Pages from './pages/Pages.jsx';
import Images from './pages/Images.jsx';
import Ota from './pages/Ota.jsx';

const NAV = [
  { to: '/devices', label: '设备管理' },
  { to: '/pages', label: '页组管理' },
  { to: '/images', label: '替换页面画面' },
  { to: '/ota', label: 'OTA' },
];

function DeviceSelector() {
  const [devices, setDevices] = useState([]);
  const [selected, setSelected] = useState(getSelectedDevice());
  const [loadErr, setLoadErr] = useState('');
  // A device switch closes whatever is being edited for the old device.
  const [pendingSwitch, setPendingSwitch] = useState(null);

  useEffect(() => {
    const sync = () => setSelected(getSelectedDevice());
    window.addEventListener('device-changed', sync);
    return () => window.removeEventListener('device-changed', sync);
  }, []);

  useEffect(() => {
    api.devices()
      .then((ds) => {
        const trusted = (ds || []).filter((d) => d.trust);
        setDevices(trusted);
        // A device that lost trust (or was removed) must not stay selected.
        if (getSelectedDevice() && !trusted.some((d) => d.device_id === getSelectedDevice())) {
          setSelectedDevice('');
        }
        setSelected(getSelectedDevice());
        setLoadErr('');
      })
      .catch(() => {
        setDevices([]);
        setSelectedDevice('');
        setLoadErr('设备列表加载失败');
      });
  }, []);

  const choose = (id) => {
    if (id !== selected && hasUnsaved()) {
      setPendingSwitch(id);
      return;
    }
    setSelectedDevice(id);
  };

  return (
    <div className="field" style={{ padding: '0 20px 8px' }}>
      <div className="field-label" id="device-selector-label">当前设备</div>
      <select
        aria-labelledby="device-selector-label"
        value={selected}
        onChange={(e) => choose(e.target.value)}
      >
        <option value="">请选择设备</option>
        {devices.map((d) => (
          <option key={d.device_id} value={d.device_id}>
            {d.device_id}
          </option>
        ))}
      </select>
      {/* `loadErr` is set after the fetch settles, so it needs a live region. */}
      <div className="err" role="alert" style={{ marginBottom: 0 }}>
        {loadErr}
      </div>
      {pendingSwitch !== null && (
        <ConfirmDialog
          title="有未保存的修改"
          body="切换设备会关闭当前正在编辑的内容，未保存的修改将丢失。"
          confirmLabel="放弃修改并切换"
          cancelLabel="留在当前设备"
          danger
          onConfirm={() => {
            setSelectedDevice(pendingSwitch);
            setPendingSwitch(null);
          }}
          onCancel={() => setPendingSwitch(null)}
        />
      )}
    </div>
  );
}

function Layout() {
  const navigate = useNavigate();
  const logout = () => {
    clearToken();
    navigate('/login');
  };
  return (
    <div className="layout">
      <a className="skip-link" href="#main">跳到主要内容</a>
      <aside className="sidebar">
        <div className="brand">Youn Ink Admin</div>
        <DeviceSelector />
        <nav aria-label="主导航">
          {NAV.map((item) => (
            <NavLink key={item.to} to={item.to} className={({ isActive }) => (isActive ? 'active' : undefined)}>
              {item.label}
            </NavLink>
          ))}
        </nav>
        <button type="button" className="btn secondary logout" onClick={logout}>退出</button>
      </aside>
      {/* tabIndex lets the skip link move focus here. */}
      <main id="main" className="content" tabIndex={-1}>
        <Outlet />
      </main>
    </div>
  );
}

function ProtectedLayout() {
  if (!isAuthed()) return <Navigate to="/login" replace />;
  return <Layout />;
}

// Consumed by createBrowserRouter in main.jsx. A layout route keeps the sidebar
// mounted across navigations, and the data router is what makes useBlocker
// available to the pages that can hold unsaved edits.
export const routes = [
  { path: '/login', element: <Login /> },
  {
    path: '/',
    element: <ProtectedLayout />,
    children: [
      { index: true, element: <Navigate to="/devices" replace /> },
      { path: 'devices', element: <Devices /> },
      { path: 'pages', element: <Pages /> },
      { path: 'images', element: <Images /> },
      { path: 'ota', element: <Ota /> },
    ],
  },
  { path: '*', element: <Navigate to={isAuthed() ? '/devices' : '/login'} replace /> },
];
