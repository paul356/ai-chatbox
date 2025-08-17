
# 项目介绍

## 项目用途

这个代码仓是托管AI-Chatbox项目软件代码的仓库，AI-Chatbox是一个专门用于和LLM对话的盒子硬件。用户可以直接使用语音向LLM提问，盒子获取LLM的回答并**使用中文语音合成播报给用户**。目标用户是那些不方便使用手机APP的人群，比如小孩、老人、障人士、还有不想使用手机APP的人士等。

## 主要功能

- 🎤 **语音输入**: 使用PDM麦克风录制用户语音
- 🔤 **语音识别**: 基于Vosk的离线中文语音转文字
- 🤖 **LLM对话**: 集成DeepSeek API进行智能对话
- 🔊 **语音输出**: **NEW!** 使用ESP-TTS进行中文语音合成
- 📱 **I2S音频**: 通过MAX98357放大器输出高质量音频
- 🔄 **实时处理**: 多线程架构确保流畅的语音交互体验

## 开发准备

首先得先准备一个ESP32S3开发板，这个开发板得有语音输入和播报功能。我当前使用XIAO ESP32S3 Sense开发板和一个外接的语音编码硬件来充当软件开发的实验平台。

### 硬件要求
- ESP32S3开发板 (推荐XIAO ESP32S3 Sense)
- PDM麦克风 (语音输入)
- MAX98357 I2S音频放大器 (语音输出)
- 扬声器
- SD卡 (存储音频文件)

然后还得有RUST on ESP环境，具体安装过程可以参考[安装RUST on ESP环境](https://paul356.github.io/2024/11/11/rust-on-esp-series_1.html)。有了这两项准备后就开始编译软件了。


## 如何编译

先进入ESPUP环境。

`source $HOME/export-esp.sh`

进入代码目录，执行 `cargo build` 命令。

编译成功后，就可以使用命令 `cargo espflash flash` 将固件上传到ESP32S3开发板上。

## TTS语音合成设置

为了启用中文语音合成功能，需要上传语音数据到设备：

1. **准备语音数据**:
   ```bash
   # 语音数据文件已包含在项目中
   ls voice_data.dat
   ```

2. **上传语音数据到设备**:
   ```bash
   ./upload-voice-data.sh /dev/ttyUSB0
   ```

3. **验证分区表**:
   确保 `partitions.csv` 包含语音数据分区：
   ```csv
   voice_data, data, spiffs, 0x290000, 2048K
   ```

更多TTS集成详情请参考 [TTS_INTEGRATION.md](TTS_INTEGRATION.md)

## 运行监控

然后就可以通过 `cargo espflash monitor` 查看运行日志。

