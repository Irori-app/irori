<script setup lang="ts">
/** The hearth's square frame: thick wood around a screenshot, warmed from below. */
</script>

<template>
  <div class="hearth-frame relative">
    <div class="relative overflow-hidden rounded-[0.9rem] bg-ash">
      <slot />
    </div>
  </div>
</template>

<style scoped>
.hearth-frame {
  padding: clamp(0.4rem, 1vw, 0.75rem);
  border-radius: 1.4rem;
  background:
    linear-gradient(180deg, color-mix(in oklab, var(--hearth) 70%, var(--wood)), var(--hearth));
  border: 1px solid var(--wood);
  box-shadow:
    0 1px 0 color-mix(in oklab, var(--paper) 10%, transparent) inset,
    0 40px 90px -30px var(--glow),
    0 18px 40px -24px rgb(0 0 0 / 0.45);
}
</style>
