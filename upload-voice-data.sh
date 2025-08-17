#!/bin/bash

# Upload voice data to ESP32S3 partition
# This script uploads the TTS voice data to the voice_data partition

DEVICE=${1:-/dev/ttyUSB0}

echo "Uploading voice data to ESP32S3..."
echo "Device: $DEVICE"

# Check if voice data file exists
if [ ! -f "voice_data.dat" ]; then
    echo "Error: voice_data.dat not found!"
    echo "Make sure to copy a voice data file from esp-sr/esp-tts/esp_tts_chinese/"
    exit 1
fi

# Get file size
FILE_SIZE=$(stat -c%s "voice_data.dat")
echo "Voice data file size: $FILE_SIZE bytes"

# Flash the voice data to the partition
python -m esptool --chip esp32s3 --port $DEVICE --baud 115200 write_flash 0x290000 voice_data.dat

echo "Voice data upload completed!"
echo ""
echo "Note: Make sure the partition table includes:"
echo "voice_data, data, spiffs, 0x290000, 2048K"
