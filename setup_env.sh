#!/bin/bash

# AI Chatbox Environment Configuration
# Source this file to set up environment variables for the AI Chatbox project
# Usage: source setup_env.sh

# WiFi Configuration
export WIFI_SSID="dummy_wifi_ssid"          # Replace with your WiFi network name
export WIFI_PASS="dummy_wifi_password"      # Replace with your WiFi password

# LLM Configuration
export LLM_AUTH_TOKEN="dummy_llm_token"     # Replace with your DeepSeek API token

# Voice Recognition Server Configuration
export VOS_URL="http://192.168.71.5:8000/transcribe"  # Replace with your Vosk server URL

echo "Environment variables set for AI Chatbox:"
echo "  WIFI_SSID: $WIFI_SSID"
echo "  WIFI_PASS: [hidden]"
echo "  LLM_AUTH_TOKEN: [hidden]"
echo "  VOS_URL: $VOS_URL"
echo ""
echo "Make sure to edit this file with your actual credentials before flashing!"
echo "To use: source setup_env.sh && cargo build"
