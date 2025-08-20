from flask import Flask, request, jsonify
from flask_cors import CORS
from vosk import Model, KaldiRecognizer
import os
import tempfile

app = Flask(__name__)
CORS(app)  # 允许跨域请求（局域网内其他设备访问）

# 加载Vosk模型（替换为你的模型路径）
model = Model("vosk-model-cn-0.22")  # 中文模型

@app.route('/transcribe', methods=['POST'])
def transcribe():
    if 'file' not in request.files:
        return jsonify({"error": "No audio file provided"}), 400

    audio_file = request.files['file']
    _, temp_path = tempfile.mkstemp(suffix=".wav")
    audio_file.save(temp_path)

    # 使用Vosk进行语音识别
    recognizer = KaldiRecognizer(model, 16000)
    with open(temp_path, 'rb') as f:
        audio_data = f.read()
        recognizer.AcceptWaveform(audio_data)

    os.remove(temp_path)  # 删除临时文件
    result = recognizer.FinalResult()
    return result["text"]
    #return jsonify({"text": result[0]})

if __name__ == '__main__':
    app.run(host='0.0.0.0', port=5000)  # 允许局域网访问
