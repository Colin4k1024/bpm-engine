import { BrowserRouter, Routes, Route } from 'react-router-dom'
import { HomePage } from './pages/HomePage'
import { TracePage } from './pages/TracePage'
import './App.css'

function App() {
  return (
    <BrowserRouter>
      <Routes>
        <Route path="/" element={<HomePage />} />
        <Route path="/trace/:instanceId" element={<TracePage />} />
      </Routes>
    </BrowserRouter>
  )
}

export default App
