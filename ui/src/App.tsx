import { BrowserRouter, Route, Routes } from 'react-router-dom'
import { AppShell } from './components/AppShell'
import { DeadLettersPage } from './pages/DeadLettersPage'
import { DefinitionsPage } from './pages/DefinitionsPage'
import { DiagnosticsPage } from './pages/DiagnosticsPage'
import { HomePage } from './pages/HomePage'
import { InstancesPage } from './pages/InstancesPage'
import { TasksPage } from './pages/TasksPage'
import { TracePage } from './pages/TracePage'
import { WorkersPage } from './pages/WorkersPage'
import './App.css'

export default function App() {
  return <BrowserRouter><AppShell><Routes>
    <Route path="/" element={<HomePage />} />
    <Route path="/definitions" element={<DefinitionsPage />} />
    <Route path="/instances" element={<InstancesPage />} />
    <Route path="/trace/:instanceId" element={<TracePage />} />
    <Route path="/tasks" element={<TasksPage />} />
    <Route path="/workers" element={<WorkersPage />} />
    <Route path="/dead-letters" element={<DeadLettersPage />} />
    <Route path="/diagnostics" element={<DiagnosticsPage />} />
  </Routes></AppShell></BrowserRouter>
}
