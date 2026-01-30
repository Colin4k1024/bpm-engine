import { useState } from 'react'
import { useNavigate } from 'react-router-dom'

export function HomePage() {
  const [instanceId, setInstanceId] = useState('')
  const navigate = useNavigate()

  const handleSubmit = (e: React.FormEvent) => {
    e.preventDefault()
    const id = instanceId.trim()
    if (id) navigate(`/trace/${encodeURIComponent(id)}`)
  }

  return (
    <div className="home-page">
      <h1>Execution Trace UI</h1>
      <p>Enter a process instance ID to view its execution trace.</p>
      <form onSubmit={handleSubmit}>
        <input
          type="text"
          value={instanceId}
          onChange={(e) => setInstanceId(e.target.value)}
          placeholder="e.g. abc12345-..."
          aria-label="Instance ID"
        />
        <button type="submit">View Trace</button>
      </form>
    </div>
  )
}
