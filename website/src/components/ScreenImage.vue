<script setup lang="ts">
import { computed } from 'vue'
import { useTheme } from '@/composables/useTheme'
import type { Screen } from '@/data/content'

const props = defineProps<{
  screen: Screen
  /** Force a theme instead of following the page's. */
  variant?: 'dark' | 'light'
  eager?: boolean
  sizes?: string
}>()

const { isDark } = useTheme()

const stem = computed(() => {
  const light = props.variant ? props.variant === 'light' : !isDark.value
  return light && props.screen.light ? `light-${props.screen.id}` : props.screen.id
})
</script>

<template>
  <img
    :src="`/screenshots/${stem}.webp`"
    :srcset="`/screenshots/${stem}.webp 1440w, /screenshots/${stem}@2x.webp 2880w`"
    :sizes="sizes ?? '(min-width: 1280px) 60vw, 100vw'"
    :alt="`Irori: ${screen.title}. ${screen.caption}`"
    width="1440"
    height="901"
    :loading="eager ? 'eager' : 'lazy'"
    :fetchpriority="eager ? 'high' : 'auto'"
    decoding="async"
    class="block h-auto w-full max-w-full"
  />
</template>
