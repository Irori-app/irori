<script setup lang="ts">
import { STORES } from '@/data/content'
</script>

<template>
  <section aria-labelledby="stores-title" class="border-y border-grain bg-soot/60">
    <div class="gutter mx-auto flex max-w-7xl flex-col gap-6 py-12 lg:flex-row lg:items-center lg:gap-12">
      <div class="flex min-w-0 flex-col gap-2 lg:w-64 lg:shrink-0">
        <p class="tag">Gathers</p>
        <h2 id="stores-title" class="font-display text-2xl font-bold text-paper">Ten stores and launchers</h2>
        <p class="text-[0.95rem] text-smoke">Plus your ROMs, your own games, and libraries imported from Heroic and Lutris.</p>
      </div>
      <!-- A shoji lattice: hairline frames between the cells. -->
      <ul class="lattice grid min-w-0 flex-1 grid-cols-2 sm:grid-cols-3 lg:grid-cols-5">
        <li v-for="store in STORES" :key="store" class="flex min-h-16 items-center bg-ash px-4 py-3 font-display text-[1.05rem] font-medium text-paper">
          {{ store }}
        </li>
      </ul>
    </div>
  </section>
</template>

<style scoped>
.lattice {
  gap: 1px;
  background: var(--grain);
  border: 1px solid var(--grain);
  border-radius: 0.9rem;
  overflow: hidden;
}
</style>
