// 刷写目标判定。分区表实测值（由 firmware/build/partition_table/partition-table.bin 解析）：
//   ota_0 @0x20000 4032K   ota_1 @0x410000 4032K   otadata @0xd000 8K
//
// 为什么需要它：OTA 更新过之后 otadata 指向 ota_1，此时往 0x20000 刷会
// "刷写成功但设备照旧跑老固件"。所以目标是"活动槽"，不是常量 0x20000。

export const SLOTS = [
  { name: 'ota_0', offset: 0x20000 },
  { name: 'ota_1', offset: 0x410000 },
];
export const OTADATA_OFFSET = 0xd000;
export const OTADATA_SIZE = 0x2000;
export const APP_PARTITION_SIZE = 0x3f0000;

// otadata 的两条槽位记录各自独占一个 4 KiB 扇区，**不是**连续两条 32 字节：
// ESP-IDF 的 bootloader_common_read_otadata() 从 ota_select_map + SPI_SEC_SIZE
// 读第二条；写入侧 rewrite_ota_seq() 整扇区擦除后只写 32 字节，0x20.. 恒为 0xFF。
// 按记录长度步进会永远只看到第一条，从第二次 OTA 起交替就判错槽：
//   ota_0 → OTA 到 ota_1（seq=2，扇区 0）→ OTA 回 ota_0（seq=3，扇区 1）
// 按 32 步进会读到过期的 seq=2 报 ota_1，而 bootloader 启动的是 ota_0。
const SECTOR_SIZE = OTADATA_SIZE / 2; // 0x1000
const ENTRY_SIZE = 32;
const ERASED = 0xffffffff;

/**
 * 解析 otadata，给出刷写目标。
 * ESP-IDF 的 seq→槽位映射**未在本项目真机上验证过**，因此结果必须连同证据一起
 * 展示，并允许用户在 UI 里手动覆盖 —— 这是 spec「已知风险」里写明的那一条。
 */
export function parseOtadata(bytes) {
  if (!bytes || bytes.byteLength < SECTOR_SIZE + ENTRY_SIZE) {
    return {
      slotIndex: 0,
      ...SLOTS[0],
      seq: null,
      evidence: 'otadata 读不到内容 ⇒ 按 ota_0 处理（请人工确认）',
      verified: false,
    };
  }
  const entries = [0, 1].map((i) => ({
    index: i,
    seq: new DataView(bytes.buffer, bytes.byteOffset + i * SECTOR_SIZE, ENTRY_SIZE).getUint32(0, true),
  }));
  const valid = entries.filter((e) => e.seq !== ERASED && e.seq !== 0);
  if (valid.length === 0) {
    return {
      slotIndex: 0,
      ...SLOTS[0],
      seq: null,
      evidence: 'otadata 为擦除态 ⇒ bootloader 回落到 ota_0',
      verified: true,
    };
  }
  const active = valid.reduce((a, b) => (b.seq > a.seq ? b : a));
  const slotIndex = (active.seq - 1) % 2;
  return {
    slotIndex,
    ...SLOTS[slotIndex],
    seq: active.seq,
    evidence: `otadata ota_seq=${active.seq} ⇒ 推断 ${SLOTS[slotIndex].name}（未真机验证，请对照开机日志的 "Loaded app from partition at offset"）`,
    verified: false,
  };
}

/** 写入参数：与项目既有 esptool 命令行一致（esp32s3 / dio / 80m / 16MB）。 */
export const FLASH_PARAMS = {
  flashMode: 'dio',
  flashFreq: '80m',
  flashSize: '16MB',
  chip: 'esp32s3',
};
