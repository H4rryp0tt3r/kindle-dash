#!/bin/bash
# Runs only in the throwaway build container. stdout is the personalised image.
set -euo pipefail
cd /s
chmod 600 seed authorized_keys rootfs.img
[ "$(wc -c < seed)" -eq 64 ] || { printf 'invalid provisioning seed\n' >&2; exit 1; }
e2fsck -fn rootfs.img >&2
label=$(dd if=rootfs.img bs=1 skip=1144 count=16 2>/dev/null | tr -d '\0')
[ "$label" = dash-root ] || { printf 'not a Dash rootfs\n' >&2; exit 1; }
mkdir verify
debugfs -R 'rdump / /s/verify' rootfs.img >/dev/null 2>&1
for dir in verify/etc/dropbear verify/var/lib/dash-ssh; do
    [ -d "$dir" ] && [ ! -L "$dir" ] && [ "$(stat -c '%a:%u:%g' "$dir")" = 700:0:0 ] || { printf 'missing private SSH directory\n' >&2; exit 1; }
    if find "$dir" -mindepth 1 | grep -q .; then
        printf 'refusing to provision an image with existing SSH state\n' >&2
        exit 1
    fi
done
cat > commands <<'EOF'
write /s/authorized_keys /etc/dropbear/authorized_keys
set_inode_field /etc/dropbear/authorized_keys mode 0100600
set_inode_field /etc/dropbear/authorized_keys uid 0
set_inode_field /etc/dropbear/authorized_keys gid 0
write /s/seed /var/lib/dash-ssh/seed
set_inode_field /var/lib/dash-ssh/seed mode 0100600
set_inode_field /var/lib/dash-ssh/seed uid 0
set_inode_field /var/lib/dash-ssh/seed gid 0
EOF
debugfs -w -f commands rootfs.img >&2
# debugfs can report an error but exit zero: prove both writes by read-back.
mkdir readback
debugfs -R 'rdump / /s/readback' rootfs.img >/dev/null 2>&1
cmp seed readback/var/lib/dash-ssh/seed
cmp authorized_keys readback/etc/dropbear/authorized_keys
for file in readback/var/lib/dash-ssh/seed readback/etc/dropbear/authorized_keys; do
    [ "$(stat -c '%a:%u:%g' "$file")" = 600:0:0 ] || { printf 'incorrect credential mode/owner\n' >&2; exit 1; }
done
e2fsck -fn rootfs.img >&2
printf 'PROVISION_OK\n' >&2
cat rootfs.img
