<script setup lang="ts">
const figures = [
  { value: '124', unit: 'MiB', label: 'resident memory, about 39 MiB of it heap' },
  { value: '0', unit: '%', label: 'CPU while it sits idle' },
  { value: '49', unit: 'MB', label: 'for the whole program, one process' },
  { value: '19', unit: 'languages', label: 'switchable without a restart' },
]
</script>

<template>
  <section aria-labelledby="numbers-title" class="border-y border-grain bg-soot/60">
    <div class="gutter mx-auto flex max-w-7xl flex-col gap-10 py-20">
      <div class="flex max-w-2xl flex-col gap-3">
        <p class="tag">Measured</p>
        <h2 id="numbers-title" class="font-display text-[clamp(1.9rem,3.6vw,2.8rem)] font-bold leading-[1.08] text-paper">
          No browser inside
        </h2>
        <p class="text-smoke">
          Irori draws its interface on your graphics card from native code, so it starts fast and
          leaves your memory to your games.
        </p>
      </div>
      <dl class="lattice grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-4">
        <div v-for="figure in figures" :key="figure.label" class="flex min-w-0 flex-col gap-2 bg-ash p-6">
          <dt class="order-2 text-[0.95rem] text-smoke">{{ figure.label }}</dt>
          <dd class="order-1 flex items-baseline gap-2 font-mono tabular-nums text-paper">
            <span class="text-5xl font-medium tracking-tight">{{ figure.value }}</span>
            <span class="text-lg text-tatami">{{ figure.unit }}</span>
          </dd>
        </div>
      </dl>
      <p class="font-mono text-xs text-smoke">Release build, a 16-game library and the window open.</p>
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
