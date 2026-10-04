package main

// Config holds settings.
type Config struct {
	Name string
}

// Load reads it.
func (c *Config) Load(path string) error {
	return nil
}

const Max = 10

func add(a, b int) int {
	return a + b
}

func main() {}
