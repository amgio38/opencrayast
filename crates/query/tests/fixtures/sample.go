package main

// Config holds settings.
type Config struct {
	Name string
}

// Load reads it.
func (c *Config) Load(path string) error {
	return nil
}

type Shape interface {
	Area() float64
}

const Max = 10

func Main() {}
