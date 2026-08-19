# Building an Azure Compute Gallery image

The Azure target builds a generalized Debian 13-based Claw OS image. Azure
creates the administrator, hostname, and SSH credentials when each VM starts.
The image contains no reusable `cos` login account.

## Output

```text
build/claw-os-azure-amd64.vhd
build/claw-os-azure-arm64.vhd
```

The VHD is:

- fixed-size VHD/VPC, not VHDX;
- aligned to a 1 MiB virtual size;
- GPT partitioned and UEFI bootable (Hyper-V Generation 2);
- configured with cloud-init's Azure datasource and WALinuxAgent;
- prepared with Hyper-V storage/network drivers in initramfs;
- generalized for Azure Compute Gallery.

The current GRUB EFI binary is not signed by Microsoft. Use a standard Gen2 VM
with Secure Boot disabled; do not mark this image as Trusted Launch until the
shim/GRUB/kernel signing pipeline exists.

## Build

Run on a native Debian/Ubuntu host of the same architecture as the desired
image. Windows users can build inside WSL2, but the repository must be cloned
into the Linux filesystem rather than `/mnt/c`.

```bash
sudo apt update
sudo apt install -y git build-essential pkg-config \
    debootstrap qemu-utils parted dosfstools mtools rsync \
    util-linux e2fsprogs grub-efi-amd64-bin grub-pc-bin
```

Install the Rust toolchain as described in
[Building the desktop image](building-desktop.md), then build one of the
supported flavors:

```bash
# Headless Azure image, 16 GiB virtual OS disk.
sudo ./build.sh azure

# Full COSMIC desktop Azure image, 50 GiB virtual OS disk.
sudo IMAGE_FLAVOR=desktop ./build.sh azure

# Optional virtual disk size override.
sudo SIZE=32G ./build.sh azure
```

On arm64, use an arm64 build host and install `grub-efi-arm64-bin` instead of
the amd64 GRUB packages. The output architecture follows the host.

The build verifies that the VHD's file length is exactly its virtual disk size
plus the 512-byte fixed-VHD footer and that its virtual size is 1 MiB aligned.

## Upload to a managed disk

Install the current Azure CLI and AzCopy, sign in, and select the subscription:

```bash
az login
az account set --subscription "<subscription-id-or-name>"
```

Set names for the source resources:

```bash
RG=claw-os-images
LOCATION=westus2
VHD=build/claw-os-azure-amd64.vhd
DISK=claw-os-azure-source-1-0-0

az group create --name "$RG" --location "$LOCATION"

VHD_SIZE=$(stat -c '%s' "$VHD")
az disk create \
    --resource-group "$RG" \
    --name "$DISK" \
    --location "$LOCATION" \
    --os-type Linux \
    --hyper-v-generation V2 \
    --sku Standard_LRS \
    --for-upload \
    --upload-size-bytes "$VHD_SIZE"

DISK_SAS=$(
    az disk grant-access \
        --resource-group "$RG" \
        --name "$DISK" \
        --access-level Write \
        --duration-in-seconds 86400 \
        --query accessSas \
        --output tsv
)

azcopy copy "$VHD" "$DISK_SAS" --blob-type PageBlob
az disk revoke-access --resource-group "$RG" --name "$DISK"
```

Always revoke access after AzCopy finishes. Until access is revoked, the disk
remains in an upload state and cannot be used as an image source.

## Publish to Azure Compute Gallery

Create a gallery and a generalized Gen2 image definition:

```bash
GALLERY=clawOsGallery
DEFINITION=claw-os
VERSION=1.0.0

az sig create \
    --resource-group "$RG" \
    --gallery-name "$GALLERY" \
    --location "$LOCATION"

az sig image-definition create \
    --resource-group "$RG" \
    --gallery-name "$GALLERY" \
    --gallery-image-definition "$DEFINITION" \
    --publisher ClawOS \
    --offer ClawOS \
    --sku trixie \
    --os-type Linux \
    --os-state Generalized \
    --hyper-v-generation V2 \
    --architecture x64 \
    --location "$LOCATION"
```

For an arm64 artifact, use `--architecture Arm64`.

Create the gallery version directly from the uploaded managed disk:

```bash
DISK_ID=$(
    az disk show \
        --resource-group "$RG" \
        --name "$DISK" \
        --query id \
        --output tsv
)

az sig image-version create \
    --resource-group "$RG" \
    --gallery-name "$GALLERY" \
    --gallery-image-definition "$DEFINITION" \
    --gallery-image-version "$VERSION" \
    --location "$LOCATION" \
    --target-regions "$LOCATION" \
    --storage-account-type Standard_LRS \
    --os-snapshot "$DISK_ID"
```

Despite the parameter name, the current Azure CLI accepts either a managed OS
disk or a snapshot resource ID through `--os-snapshot`; this is the managed
disk form used by the CLI's image-version examples.

Additional target regions can be added to `--target-regions`. Wait until the
version's provisioning state is `Succeeded` before deleting the source disk.

## Create a VM

```bash
IMAGE_ID=$(
    az sig image-version show \
        --resource-group "$RG" \
        --gallery-name "$GALLERY" \
        --gallery-image-definition "$DEFINITION" \
        --gallery-image-version "$VERSION" \
        --query id \
        --output tsv
)

az vm create \
    --resource-group "$RG" \
    --name claw-os-test \
    --location "$LOCATION" \
    --image "$IMAGE_ID" \
    --security-type Standard \
    --admin-username clawadmin \
    --ssh-key-values "$HOME/.ssh/id_ed25519.pub"
```

At first boot, cloud-init creates `clawadmin`, installs the SSH key, sets the
hostname and network configuration, grows the root filesystem, and generates
new machine and SSH identities. WALinuxAgent then handles Azure fabric status
and VM extensions.

### Desktop flavor login

The SSH-key deployment above intentionally creates no reusable local password,
so it is the recommended headless configuration. A graphical greeter cannot
authenticate an SSH key. For `IMAGE_FLAVOR=desktop`, first connect over SSH and
set a local password:

```bash
ssh clawadmin@<vm-address>
sudo passwd clawadmin
```

Azure does not expose a VMware-style interactive graphical console. Install and
secure a remote desktop transport such as xrdp through your deployment
configuration before expecting to use the COSMIC session remotely. Neither a
password nor an Internet-facing remote desktop service is baked into the
generalized image.
