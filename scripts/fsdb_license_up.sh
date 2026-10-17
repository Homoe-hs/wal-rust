#!/bin/sh
# 拉起"宿主原生读 FSDB"的许可通道(幂等)。
#
# 现行流程(2026-10 起): Debian 13 + KVM 硬件虚拟化虚拟机(域 edalab),
# 许可 daemon 在客机里, 宿主经 libvirt NAT 直连 `27080@192.168.122.50`。
# 旧的 NixOS/TCG 流程(run25.sh + lic25_hostfwd.sh + 127.0.0.1 hostfwd)已废弃。
#
# 用法:
#   bash scripts/fsdb_license_up.sh        # 幂等: VM 检查 → 许可 daemon → 宿主连通性
#   . .tools/vm/host_fsdb_env.sh           # 宿主环境(VERDI_HOME / SNPSLMD_LICENSE_FILE)
#   target/release/wal-rust '(length (SIGNALS))' -l x.fsdb
set -u
VHOST=${WAL_LICENSE_HOST:-192.168.122.50}
VMPORT=${WAL_LICENSE_PORT:-27080}
SSH="ssh -F /dev/null -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o ConnectTimeout=8 root@$VHOST"
VM_START="$HOME/Projects/fsdb-parser/system-dev-vm/os/debian13/start.sh"

echo "== 1) 许可 VM 可达性 ($VHOST)"
if ! $SSH 'true' 2>/dev/null; then
  echo "VM 不可达, 尝试启动: bash $VM_START"
  [ -x "$VM_START" ] || [ -f "$VM_START" ] || { echo "找不到启动脚本: $VM_START"; exit 1; }
  bash "$VM_START" >/dev/null 2>&1 &
  i=0
  while [ $i -lt 90 ]; do
    if $SSH 'true' 2>/dev/null; then break; fi
    sleep 2; i=$((i+2))
  done
  $SSH 'true' 2>/dev/null || { echo "✗ VM 仍不可达(见 docs/fsdb-env.md §4)"; exit 1; }
  echo "VM 已启动 (${i}s)"
fi

echo "== 2) 客机许可 daemon(幂等: 脚本自带 pkill + 端口等待 + 自检)"
$SSH 'bash /mnt/vmrepo/os/debian13/vm_lic25.sh 2>&1 | tail -4' 2>/dev/null | sed 's/^/   /'

echo "== 3) 宿主侧连通性 $VHOST:$VMPORT"
ok=0
i=0
while [ $i -lt 20 ]; do
  if timeout 5 bash -c "cat < /dev/null > /dev/tcp/$VHOST/$VMPORT" 2>/dev/null; then ok=1; break; fi
  sleep 2; i=$((i+2))
done
if [ "$ok" = 1 ]; then
  echo "✓ 许可通道可用: $VMPORT@$VHOST"
  echo "  宿主用法: . .tools/vm/host_fsdb_env.sh  (或手动 export SNPSLMD_LICENSE_FILE=$VMPORT@$VHOST)"
else
  echo "✗ $VHOST:$VMPORT 连不上, 别继续跑 FSDB 测试(见 docs/fsdb-env.md §5)"
  exit 1
fi
