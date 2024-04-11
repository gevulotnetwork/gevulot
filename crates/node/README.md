# Gevulot Node

**Work in Progress**

## Initial setup

Preliminary assumptions:
- Linux operating system
- x86_64 processor
- Rust installed

### Dependencies

- `cuda-opencl-<ver>`
- `cuda-opencl-devel-<ver>`
- `hwlock-devel`
- `ocl-icd-devel`
- `protobuf`
- `protobuf-c`
- `protobuf-compiler`
- `protobuf-devel`

- `cargo install sqlx-cli`

### Gevulot working directory

`mkdir -p /var/lib/gevulot`

Also ensure appropriate rights for that directory.

### Database

#### Initialization
- `podman-compose up`
- `cargo sqlx database create --database-url postgres://gevulot:gevulot@localhost/gevulot`
- `cargo sqlx migrate run --database-url postgres://gevulot:gevulot@localhost/gevulot`

#### Refresh SQLX cache
- `cargo sqlx prepare --database-url postgres://gevulot:gevulot@localhost/gevulot`

### GPU Passthrough

#### nVidia driver adjustments for Gevulot

##### Blacklist nVidia modules
**/etc/modprobe.d/nvidia.conf**
```
blacklist nvidia
blacklist nvidia-drm
```

##### Load the VFIO modules
**/etc/modules-load.d/vfio.conf
```
kvmgt
vfio-pci
vfio-iommu-type1
vfio-mdev
```

##### Configure vfio-pci passthrough devices

First find out PCI IDs:
```
$ lspci -nn  | grep -i nvidia
01:00.0 VGA compatible controller [0300]: NVIDIA Corporation TU104 [GeForce RTX 2070 SUPER] [10de:1e84] (rev a1)
01:00.1 Audio device [0403]: NVIDIA Corporation TU104 HD Audio Controller [10de:10f8] (rev a1)
01:00.2 USB controller [0c03]: NVIDIA Corporation TU104 USB 3.1 Host Controller [10de:1ad8] (rev a1)
01:00.3 Serial bus controller [0c80]: NVIDIA Corporation TU104 USB Type-C UCSI Controller [10de:1ad9] (rev a1)
```

Then add all the GPU PCI IDs into module config:
**/etc/modprobe.d/vfio.conf**
```
options vfio-pci ids=10de:1e84,10de:10f8,10de:1ad8,10de:1ad9
```

##### Adjust vfio device permissions with udev rule

**/etc/udev/rules.d/99-vfio.rules**
```
SUBSYSTEM=="vfio", OWNER="root", GROUP="kvm"
```

#### Fix GPU USB-controller binding:

It is possible that `xhci_hcd` driver is built into kernel and it's impossible
to prevent it from binding GPU's USB controller, so it must be fixed after boot.

```
echo "0000:01:00.2" | sudo tee /sys/bus/pci/drivers/xhci_hcd/unbind
echo "0000:01:00.2" | sudo tee /sys/bus/pci/drivers/vfio-pci/bind
```

#TEST
