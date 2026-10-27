<script setup lang="ts">
import { ArrowUpRight } from '@lucide/vue'
import BrandLockup from '@/components/BrandLockup.vue'
import ThemeToggle from '@/components/ThemeToggle.vue'
import { REPO } from '@/data/content'

const links = [
  { href: '/#features', label: 'Features' },
  { href: '/#screenshots', label: 'Screenshots' },
  { href: '/#download', label: 'Download' },
  { href: '/#faq', label: 'Questions' },
]
</script>

<template>
  <header class="site-header sticky z-40 border-b border-grain/70 backdrop-blur-md">
    <div class="gutter mx-auto flex h-16 max-w-7xl items-center gap-6">
      <a href="/" class="text-paper" aria-label="Irori home">
        <BrandLockup id="header" />
      </a>
      <nav aria-label="Sections" class="ml-auto hidden items-center gap-1 md:flex">
        <a
          v-for="link in links"
          :key="link.href"
          :href="link.href"
          class="rounded-full px-3 py-1.5 text-[0.95rem] text-smoke transition-colors hover:bg-soot hover:text-paper"
        >{{ link.label }}</a>
        <a
          :href="REPO"
          class="inline-flex items-center gap-1 rounded-full px-3 py-1.5 text-[0.95rem] text-smoke transition-colors hover:bg-soot hover:text-paper"
        >GitHub <ArrowUpRight class="size-3.5" /></a>
      </nav>
      <ThemeToggle class="ml-auto md:ml-0" />
    </div>
  </header>
</template>

<style scoped>
.site-header {
  top: env(safe-area-inset-top, 0px);
  background: color-mix(in oklab, var(--ash) 82%, transparent);
}
</style>
