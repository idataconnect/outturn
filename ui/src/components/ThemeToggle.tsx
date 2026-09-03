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
      className="flex gap-0.5 p-0.5 rounded-md bg-gray-100 dark:bg-gray-800"
    >
      {options.map(({ value, icon: Icon, label }) => (
        <button
          key={value}
          role="radio"
          aria-checked={theme === value}
          aria-label={label}
          title={label}
          onClick={() => setTheme(value)}
          className={`flex-1 flex items-center justify-center py-1.5 rounded transition-colors ${
            theme === value
              ? 'bg-white dark:bg-gray-950 text-gray-900 dark:text-gray-100 shadow-sm'
              : 'text-gray-500 dark:text-gray-400 hover:text-gray-900 dark:hover:text-gray-100'
          }`}
        >
          <Icon size={14} />
        </button>
      ))}
    </div>
  )
}
