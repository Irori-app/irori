import { computed, ref, watchEffect } from 'vue'

export type ThemeChoice = 'auto' | 'light' | 'dark'

const KEY = 'irori-theme'
const media = window.matchMedia('(prefers-color-scheme: dark)')

function stored(): ThemeChoice {
  try {
    const value = localStorage.getItem(KEY)
    return value === 'light' || value === 'dark' ? value : 'auto'
  } catch {
    return 'auto'
  }
}

const choice = ref<ThemeChoice>(stored())
const systemDark = ref(media.matches)
media.addEventListener('change', (event) => (systemDark.value = event.matches))

const isDark = computed(() => choice.value === 'dark' || (choice.value === 'auto' && systemDark.value))

watchEffect(() => {
  document.documentElement.classList.toggle('dark', isDark.value)
  try {
    if (choice.value === 'auto') localStorage.removeItem(KEY)
    else localStorage.setItem(KEY, choice.value)
  } catch {
    // Private windows may refuse storage; the theme still applies for this visit.
  }
})

/** The page's theme: the visitor's choice, or the system's when they haven't made one. */
export function useTheme() {
  return { choice, isDark }
}
