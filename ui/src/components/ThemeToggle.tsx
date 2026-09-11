import { Monitor, Moon, Sun } from 'lucide-react'

import { useTheme } from '../lib/useTheme'
import type { Theme } from '../lib/theme'

const options: { value: Theme; icon: typeof Sun; label: string }[] = [
  { value: 'light', icon: Sun, label: 'Light' },
  { value: 'dark', icon: Moon, label: 'Dark' },
  { value: 'system', icon: Monitor, label: 'System' },
]

/**
 * A segmented control rather than a two-way switch: following the system is a
 * third choice, and a toggle cannot express it.
 */
export default function ThemeToggle() {
  const [theme, setTheme] = useTheme()

  return (
    <div
      role="radiogroup"
      aria-label="Colour theme"
      className="flex gap-0.5 p-0.5 rounded-md bg-surface-100 dark:bg-surface-800"
    >
      {options.map(({ value, icon: Icon, label }) => (
        <button
          key={value}
          role="radio"
          aria-checked={theme === value}
          aria-label={label}
          title={label}
          // A radio group is one stop, not three: tab reaches the chosen
          // option and the arrows move within. Without this the roles say
          // radio group and the keyboard behaves like a row of buttons.
          tabIndex={theme === value ? 0 : -1}
          onKeyDown={(e) => {
            const step = e.key === 'ArrowRight' || e.key === 'ArrowDown' ? 1 : e.key === 'ArrowLeft' || e.key === 'ArrowUp' ? -1 : 0
            if (step === 0) return
            e.preventDefault()
            const at = options.findIndex((o) => o.value === theme)
            const next = options[(at + step + options.length) % options.length]
            setTheme(next.value)
            // Selection follows focus, as it does in a native group, so the
            // focused option is the chosen one.
            e.currentTarget.parentElement
              ?.querySelectorAll('button')
              [options.indexOf(next)]?.focus()
          }}
          onClick={() => setTheme(value)}
          className={`flex-1 flex items-center justify-center py-1.5 rounded transition-colors ${
            theme === value
              ? 'bg-white dark:bg-surface-700 text-surface-900 dark:text-surface-100 shadow-sm'
              : 'text-surface-600 dark:text-surface-400 hover:text-surface-900 dark:hover:text-surface-100'
          }`}
        >
          <Icon size={14} />
        </button>
      ))}
    </div>
  )
}
