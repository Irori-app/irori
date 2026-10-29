<script setup lang="ts">
import BrandLockup from '@/components/BrandLockup.vue'
import { REPO } from '@/data/content'

const links = [
  { href: REPO, label: 'Source code' },
  { href: `${REPO}/releases`, label: 'Releases' },
  { href: `${REPO}/issues`, label: 'Report a problem' },
  { href: `${REPO}/blob/main/LICENSE`, label: 'MIT licence' },
  { href: '/code-signing', label: 'Code signing policy' },
]
</script>

<template>
  <footer class="border-t border-grain">
    <div class="gutter mx-auto flex max-w-7xl flex-col gap-8 py-14">
      <div class="flex flex-col gap-6 sm:flex-row sm:items-center sm:justify-between">
        <span class="text-paper"><BrandLockup id="footer" /></span>
        <nav aria-label="Project" class="flex flex-wrap gap-x-6 gap-y-2">
          <a v-for="link in links" :key="link.label" :href="link.href" class="text-[0.95rem] text-smoke hover:text-paper">{{ link.label }}</a>
        </nav>
      </div>
      <p class="max-w-3xl text-sm leading-relaxed text-smoke">
        Irori is free software, made independently. It is not affiliated with or endorsed by Epic
        Games, GOG, Amazon, ZOOM Platform, itch.io, Valve, Blizzard, Electronic Arts, Ubisoft or
        Rockstar Games; their names belong to their owners. Games shown in screenshots are
        fictional.
      </p>
    </div>
  </footer>
</template>
