import React, { useEffect, useState } from 'react';
import { Routes, Route, Navigate, useNavigate, Link } from 'react-router-dom';
import { isAuthed, clearToken } from './auth.js';
import { api } from './api.js';
import { getSelectedDevice, setSelectedDevice } from './deviceContext.js';
import Login from './pages/Login.jsx';
import Devices from './pages/Devices.jsx';
import Pages from './pages/Pages.jsx';
import Images from './pages/Images.jsx';
import Ota from './pages/Ota.jsx';

function DeviceSelector() {
  const [devices, setDevices] = useState([]);
  const [selected, setSelected] = useState(getSelectedDevice());
  const [loadErr, setLoadErr] = useState('');
  useEffect(() => {
    const sync = () => setSelected(getSelectedDevice());
    window.addEventListener('device-changed', sync);
    return () => window.removeEventListener('device-changed', sync);
  }, []);
  useEffect(() => {
    api.devices().then((ds) => {
      const trusted = (ds || []).filter((d) => d.trust);
      setDevices(trusted);
      if (getSelectedDevice() && !trusted.some((d) => d.device_id === getSelectedDevice())) {
        setSelectedDevice('');
      }
      setSelected(getSelectedDevice());
      setLoadErr('');
    }).catch(() => {
      setDevices([]);
      setSelectedDevice('');
      setLoadErr('设备列表加载失败');
    });
  }, []);
  return (
    <div className="row" style={{ padding: '0 12px 8px' }}>
      <label style={{ flex: 1 }}>当前设备
        <select value={selected} onChange={(e) => setSelectedDevice(e.target.value)} style={{ width: '100%' }}>
          <option value="">请选择设备</option>
          {devices.map((d) => (
            <option key={d.device_id} value={d.device_id}>{d.device_id}</option>
          ))}
        </select>
      </label>
      {loadErr && <div className="err" style={{ fontSize: 12 }}>{loadErr}</div>}
    </div>
  );
}

function Layout({ children }) {
  const nav = useNavigate();
  const logout = () => { clearToken(); nav('/login'); };
  return (
    <div className="layout">
      <aside className="sidebar">
        <div className="brand">Youn Ink Admin</div>
        <DeviceSelector />
        <nav>
          <Link to="/devices">设备管理</Link>
          <Link to="/pages">页组管理</Link>
          <Link to="/images">替换页面画面</Link>
          <Link to="/ota">OTA</Link>
        </nav>
        <button className="btn logout" onClick={logout}>退出</button>
      </aside>
      <main className="content">{children}</main>
    </div>
  );
}

function Protected({ children }) {
  if (!isAuthed()) return <Navigate to="/login" replace />;
  return <Layout>{children}</Layout>;
}

export default function App() {
  return (
    <Routes>
      <Route path="/login" element={<Login />} />
      <Route path="/devices" element={<Protected><Devices /></Protected>} />
      <Route path="/pages" element={<Protected><Pages /></Protected>} />
      <Route path="/images" element={<Protected><Images /></Protected>} />
      <Route path="/ota" element={<Protected><Ota /></Protected>} />
      <Route path="*" element={<Navigate to={isAuthed() ? '/devices' : '/login'} replace />} />
    </Routes>
  );
}
