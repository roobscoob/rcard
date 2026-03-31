# Enumerate COM ports over USB, filtered by Vendor ID
# Usage: .\Get-UsbComPorts.ps1 -VendorId "1A86"

param(
    [Parameter(Mandatory = $true)]
    [string]$VendorId
)

$pattern = "VID_$VendorId"

Get-CimInstance -ClassName Win32_PnPEntity |
    Where-Object {
        $_.Name -match 'COM\d+' -and
        $_.DeviceID -match "VID_$VendorId"
    } |
    ForEach-Object {
        if ($_.Name -match '\((COM\d+)\)') { $Matches[1] }
    }