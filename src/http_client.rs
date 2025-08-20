use anyhow;
use esp_idf_svc::http::client::{EspHttpConnection};
use esp_idf_svc::http::Method;

/// Helper function to send a multipart request with a file
pub fn send_multipart_request(
    client: &mut EspHttpConnection,
    url: &str,
    file_path: &str,
    file_data: &[u8],
) -> anyhow::Result<()> {
    // Create multipart form data boundary
    let boundary = "------------------------boundary";

    // Create request body
    let request_body = create_multipart_body(boundary, file_path, file_data);

    // Set up headers
    let content_type = format!("multipart/form-data; boundary={}", boundary);
    let content_length = request_body.len().to_string();

    let headers = [
        ("Content-Type", content_type.as_str()),
        ("Content-Length", content_length.as_str()),
    ];

    // Send the request
    if let Err(e) = client.initiate_request(Method::Post, url, &headers) {
        return Err(anyhow::anyhow!("Failed to initiate HTTP request: {}", e));
    }

    // Write the request body
    if let Err(e) = client.write(&request_body) {
        return Err(anyhow::anyhow!("Failed to write request body: {}", e));
    }

    // Finalize the request
    if let Err(e) = client.initiate_response() {
        return Err(anyhow::anyhow!("Failed to get response: {}", e));
    }

    Ok(())
}

/// Helper function to create a multipart request body
fn create_multipart_body(boundary: &str, file_path: &str, file_data: &[u8]) -> Vec<u8> {
    let filename = file_path.split('/').last().unwrap_or("audio.wav");
    let content_disposition = format!(
        "Content-Disposition: form-data; name=\"file\"; filename=\"{}\"\r\n",
        filename
    );
    let content_type = "Content-Type: audio/wav\r\n\r\n";

    let mut request_body = Vec::new();

    // Add boundary start
    request_body.extend_from_slice(format!("--{}\r\n", boundary).as_bytes());

    // Add content disposition
    request_body.extend_from_slice(content_disposition.as_bytes());

    // Add content type
    request_body.extend_from_slice(content_type.as_bytes());

    // Add file data
    request_body.extend_from_slice(file_data);
    request_body.extend_from_slice(b"\r\n");

    // Add boundary end
    request_body.extend_from_slice(format!("--{}--\r\n", boundary).as_bytes());

    request_body
}

/// Helper function to read response body
pub fn read_response_body(client: &mut EspHttpConnection) -> anyhow::Result<String> {
    let mut response_body = Vec::new();
    let mut buffer = [0u8; 1024];

    loop {
        match client.read(&mut buffer) {
            Ok(bytes_read) => {
                if bytes_read == 0 {
                    break;
                }
                response_body.extend_from_slice(&buffer[..bytes_read]);
            }
            Err(e) => {
                return Err(anyhow::anyhow!("Error reading response: {}", e));
            }
        }
    }

    Ok(String::from_utf8_lossy(&response_body).to_string())
}

/// Helper function to read and process HTTP response
pub fn read_response(client: &mut EspHttpConnection) -> anyhow::Result<String> {
    // Get status code
    let status = client.status();
    log::info!("Response status: {}", status);

    if status != 200 {
        // Handle error response
        let error_text = read_response_body(client)?;
        return Err(anyhow::anyhow!("API error ({}): {}", status, error_text));
    }

    // Read successful response
    read_response_body(client)
}
