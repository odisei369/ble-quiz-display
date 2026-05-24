/* nRF52840 — full chip available after SWD erase (no S140, no UF2 bootloader). */
MEMORY
{
  FLASH : ORIGIN = 0x00000000, LENGTH = 1024K
  RAM   : ORIGIN = 0x20000000, LENGTH = 256K
}
