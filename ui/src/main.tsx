import { StrictMode } from 'react'
import { createRoot } from 'react-dom/client'
import './index.css'
import App from './App.tsx'
import { loadBrand } from './lib/brand'

// Branding first, then the first render: the page stays empty for the moment
// the config takes rather than drawing one name and then another.
void loadBrand().then(() => {
  createRoot(document.getElementById('root')!).render(
    <StrictMode>
      <App />
    </StrictMode>,
  )
})
