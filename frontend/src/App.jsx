import React from 'react';
import { Routes, Route, Navigate, useNavigate, Link } from 'react-router-dom';
import { isAuthed, clearToken } from './auth.js';
import Login from './pages/Login.jsx';
import Devices from './pages/Devices.jsx';
import Pages from './pages/Pages.jsx';
import Images from './pages/Images.jsx';
import Ota from './pages/Ota.jsx';

function Layout({ children }) {
  const nav = useNavigate();
  const logout = () => { clearToken(); nav('/login'); };
  return (
    <div className="layout">
      <aside className="sidebar">
        <div className="brand">Youn Ink Admin</div>
        <nav>
          <Link to="/devices">设备管理</Link>
          <Link to="/pages">页组管理</Link>
          <Link to="/images">图片推送</Link>
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
